import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  createBrowserGoogletagAdapter,
  createNoopGoogletagAdapter,
  type GoogletagAdapter,
  type GoogletagBindingStatus,
  type GoogletagFacade,
} from '../../src/adapters/googletag';
import {
  createBrowserMessagingAdapter,
  createNoopMessagingAdapter,
  type CaptureMessageListener,
  type MessagingAdapter,
} from '../../src/adapters/messaging';
import {
  createNoopPrebidAdapter,
  type PrebidAdapter,
  type PrebidBindingStatus,
  type PrebidEventFacade,
  type PrebidFacade,
  type PrebidTrustedServerAuctionV1,
  type PreparedTrustedBidV1,
} from '../../src/adapters/prebid';
import {
  createBrowserComposition,
  createNoopBrowserComposition,
  createTestBrowserRuntimeComposition,
} from '../../src/composition/browser';
import { log as localLog } from '../../src/core/log';
import type { BrowserAuctionBidV1 } from '../../src/core/types';
import { createCreativeIntegrationRegistration } from '../../src/integrations/creative/module';
import { createGptIntegrationRegistration } from '../../src/integrations/gpt/module';
import { isGuardInstalled, resetGuardState } from '../../src/integrations/gpt/script_guard';
import { createPrebidIntegrationRegistration } from '../../src/integrations/prebid/module';
import { publicLog } from '../../src/kernel/fallback';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';
import {
  createRenderAttempt,
  type CommittedRenderArtifact,
  type RenderAttempt,
} from '../../src/services/render';

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

function fakeGoogletagAdapter(
  bindingStatus: () => GoogletagBindingStatus = () => 'pending'
): GoogletagAdapter {
  return Object.freeze({ ...createNoopGoogletagAdapter(), bindingStatus });
}

function synchronousGptAdapter() {
  const listeners = new Map<string, Set<(event: unknown) => void>>();
  const targeting = new WeakMap<object, Map<string, readonly string[]>>();
  const bindingToken = Object.freeze({});
  const refresh = vi.fn();
  const facade: GoogletagFacade = Object.freeze({
    bindingToken: () => bindingToken,
    clearTargeting: vi.fn((slot: object, key?: string) => {
      const values = targeting.get(slot);
      if (key === undefined) values?.clear();
      else values?.delete(key);
    }),
    display: vi.fn(),
    getTargeting: vi.fn((slot: object, key: string) =>
      Object.freeze([...(targeting.get(slot)?.get(key) ?? [])])
    ),
    observeTargeting: () => Object.assign(vi.fn(), { isCurrent: () => true }),
    refresh,
    serviceState: () =>
      Object.freeze({ apiReady: true, initialLoadDisabled: false, pubadsReady: true }),
    setTargeting: vi.fn((slot: object, key: string, value: string | readonly string[]) => {
      const values = targeting.get(slot) ?? new Map<string, readonly string[]>();
      targeting.set(slot, values);
      values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
    }),
    slots: () => Object.freeze([]),
    subscribe: (eventType: string, listener: (event: unknown) => void) => {
      const registered = listeners.get(eventType) ?? new Set();
      registered.add(listener);
      listeners.set(eventType, registered);
      return () => registered.delete(listener);
    },
    transactionalReplace: () => Object.freeze({ status: 'destroyed' as const }),
  });
  const adapter: GoogletagAdapter = Object.freeze({
    bindingStatus: () => 'present',
    dispose: vi.fn(),
    notifyReady: vi.fn(),
    observePublisherCalls: () => vi.fn(),
    run: <Value>(command: (gpt: Readonly<GoogletagFacade>) => Value) => {
      let result: Promise<Value>;
      try {
        result = Promise.resolve(command(facade));
      } catch (error) {
        result = Promise.reject(error);
      }
      return Object.freeze({ status: 'present' as const, result, dispose: vi.fn() });
    },
  });
  return {
    adapter,
    emit: (eventType: string, event: unknown): void => {
      for (const listener of listeners.get(eventType) ?? []) listener(event);
    },
    refresh,
  };
}

function fakePrebidAdapter(
  bindingStatus: () => PrebidBindingStatus = () => 'pending'
): PrebidAdapter {
  return Object.freeze({ ...createNoopPrebidAdapter(), bindingStatus });
}

function synchronousPrebidAdapter() {
  let auctionListener: ((auction: Readonly<PrebidTrustedServerAuctionV1>) => void) | undefined;
  let auctionEndListener:
    ((event: unknown, prebid: Readonly<PrebidEventFacade>) => void) | undefined;
  let admitted: Readonly<PreparedTrustedBidV1> | undefined;
  const admitTrustedBid = vi.fn((prepared: Readonly<PreparedTrustedBidV1>) => {
    admitted = prepared;
    return 'admitted' as const;
  });
  const facade = Object.freeze({
    addAdUnits: vi.fn(),
    highestBids: vi.fn(() => Object.freeze([])),
    processQueue: vi.fn(),
    registerBidAdapter: vi.fn(),
    registerTrustedServerBidder: vi.fn(
      (listener: (auction: Readonly<PrebidTrustedServerAuctionV1>) => void) => {
        auctionListener = listener;
        return () => {
          auctionListener = undefined;
        };
      }
    ),
    renderAd: vi.fn(),
    requestBids: vi.fn(),
    subscribe: vi.fn(
      (
        eventType: string,
        listener: (event: unknown, prebid: Readonly<PrebidEventFacade>) => void
      ) => {
        if (eventType === 'auctionEnd') auctionEndListener = listener;
        return () => {
          if (auctionEndListener === listener) auctionEndListener = undefined;
        };
      }
    ),
  }) satisfies PrebidFacade;
  const adapter = Object.freeze({
    ...createNoopPrebidAdapter(),
    admitTrustedBid,
    bindingStatus: () => 'present' as const,
    run: <Value>(command: (prebid: Readonly<PrebidFacade>) => Value) => {
      let result: Promise<Value>;
      try {
        result = Promise.resolve(command(facade));
      } catch (error) {
        result = Promise.reject(error);
      }
      return Object.freeze({ status: 'present' as const, result, dispose: vi.fn() });
    },
  }) satisfies PrebidAdapter;
  return {
    adapter,
    admitTrustedBid,
    auction: (auction: Readonly<PrebidTrustedServerAuctionV1>): void => auctionListener?.(auction),
    auctionEnd: (auctionId: string): void => {
      const prepared = admitted;
      const highest = prepared
        ? Object.freeze([
            Object.freeze({
              ...prepared.bid,
              adUnitCode: prepared.adUnitCode,
              auctionId: prepared.auctionId,
            }),
          ])
        : Object.freeze([]);
      auctionEndListener?.(
        Object.freeze({ auctionId }),
        Object.freeze({ highestBids: () => highest })
      );
    },
  };
}

function fakeMessagingAdapter(
  installCaptureListener: MessagingAdapter['installCaptureListener'] = () => vi.fn()
): MessagingAdapter {
  return Object.freeze({ ...createNoopMessagingAdapter(), installCaptureListener });
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
    expect(composition.adapters.googletag.bindingStatus()).toBe('incompatible');
    expect(composition.adapters.prebid.bindingStatus()).toBe('incompatible');

    target.googletag = 1;
    target.pbjs = 'not-prebid';
    expect(composition.adapters.googletag.bindingStatus()).toBe('incompatible');
    expect(composition.adapters.prebid.bindingStatus()).toBe('incompatible');
  });

  it('routes the prospective first-display measure through the concrete test composition', async () => {
    const display = vi.fn();
    const pubadsService = {};
    const performance = { mark: vi.fn(), measure: vi.fn() };
    const target = {
      ...createTarget(),
      googletag: {
        apiReady: true,
        cmd: {
          push: (command: () => void): number => {
            command();
            return 1;
          },
        },
        display,
        pubads: vi.fn(() => pubadsService),
      },
      performance,
    };
    const composition = createBrowserComposition({ target });

    target.googletag.display('publisher-slot');
    expect(performance.mark).not.toHaveBeenCalled();

    await expect(
      composition.adapters.googletag.run((gpt) => {
        gpt.display('authoritative-slot');
        gpt.display('replay-slot');
      }).result
    ).resolves.toBeUndefined();

    expect(performance.mark).toHaveBeenCalledExactlyOnceWith('tsjs:first-display');
    expect(performance.measure).toHaveBeenCalledExactlyOnceWith(
      'tsjs:boot-to-first-display',
      'tsjs:bids-script',
      'tsjs:first-display'
    );
    expect(display).toHaveBeenCalledTimes(3);
  });

  it('routes an attributable empty GPT cycle through the owned slot and PUC services', async () => {
    const gpt = synchronousGptAdapter();
    let prefix = 0;
    const reservationId = `r1_${'a'.repeat(22)}`;
    const source = Object.freeze({
      type: 'adm' as const,
      version: 1 as const,
      adm: '<main>fictional fallback</main>',
      width: 300,
      height: 250,
    });
    const bid = Object.freeze({
      candidateId: 'AAAAAAAAAAAA',
      slot: 'slot-one',
      provider: 'trusted',
      upstreamBidId: 'upstream-one',
      cpm: 1,
      currency: 'USD' as const,
      targeting: Object.freeze({ hb_bidder: 'trusted' }),
      rendererReservationId: reservationId,
      renderSource: source,
    });
    const projection = Object.freeze({
      version: 1,
      auction: Object.freeze({
        version: 1,
        auctionId: 'initial',
        results: Object.freeze([
          Object.freeze({
            slot: 'slot-one',
            outcome: 'winner' as const,
            candidateId: bid.candidateId,
          }),
        ]),
      }),
      bids: Object.freeze([bid]),
    });
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
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: gpt.adapter,
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
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

    const session = composition.runtimeSessionForTest();
    const navigation = session?.currentNavigation;
    const batch = navigation?.createAuctionBatch('gpt-primary');
    const services = session?.interfaces;
    const artifacts = services?.['artifacts'];
    const reservations = composition.reservationServiceForTest();
    const slots = composition.slotServiceForTest();
    if (!navigation || !batch || !artifacts || !reservations || !slots) {
      throw new Error('Expected runtime-owned GPT dependencies');
    }
    const createAttempt = (parentAttemptId?: string): RenderAttempt => {
      const owner = batch.createRenderAttempt('slot-one');
      if (!owner.ok) throw new Error(owner.reason);
      const attempt = createRenderAttempt({
        artifacts: artifacts as Parameters<typeof createRenderAttempt>[0]['artifacts'],
        owner: owner.value,
        prepareRenderSource: (candidate) =>
          typeof candidate === 'object' && candidate !== null && Object.isFrozen(candidate)
            ? (candidate as Readonly<{ type: 'aps' | 'adm' | 'cache'; version: 1 }>)
            : undefined,
        reservations,
        ...(parentAttemptId === undefined ? {} : { parentAttemptId }),
      });
      if (!attempt.ok) throw new Error(attempt.reason);
      return attempt.value;
    };
    const ownerResult = batch.createRenderAttempt('slot-one');
    if (!ownerResult.ok) throw new Error(ownerResult.reason);
    const primaryResult = createRenderAttempt({
      artifacts: artifacts as Parameters<typeof createRenderAttempt>[0]['artifacts'],
      owner: ownerResult.value,
      prepareRenderSource: () => source,
      reservations,
    });
    if (!primaryResult.ok) throw new Error(primaryResult.reason);
    const primary = primaryResult.value;
    const physicalSlot = Object.freeze({});
    const slotElement = document.createElement('div');
    slotElement.id = 'slot-one';
    document.body.append(slotElement);
    expect(
      slots.adoptGptSlot(navigation.generation, 'slot-one', {
        definition: {
          adUnitPath: '/123/slot-one',
          elementId: 'slot-one',
          sizes: Object.freeze([[300, 250]]),
        },
        ownership: 'trusted_server',
        slot: physicalSlot,
      })
    ).toEqual({ ok: true });
    const artifact = Object.freeze({
      kind: 'puc' as const,
      attemptId: primary.id,
      slot: primary.slot,
      navigationGeneration: primary.navigationGeneration,
      dispose: vi.fn(),
    }) satisfies CommittedRenderArtifact;
    const projectedBid = (
      navigation.currentAuctionProjection as Readonly<{ bids: readonly BrowserAuctionBidV1[] }>
    ).bids[0];
    if (!projectedBid) throw new Error('Expected the parsed projected winner');
    let fallback: RenderAttempt | undefined;
    const operation = await composition.publishGptWinnerForTest({
      artifact,
      attempt: primary,
      bid: projectedBid,
      createFallback: (parentAttemptId) => {
        fallback = createAttempt(parentAttemptId);
        return Object.freeze({ ok: true as const, value: fallback });
      },
      operation: 'refresh',
      owner: ownerResult.value,
      requestClass: 'primary',
      slot: physicalSlot,
    });
    expect(operation.ok).toBe(true);

    await Promise.resolve();
    await Promise.resolve();
    expect(gpt.refresh).toHaveBeenCalledExactlyOnceWith(
      [physicalSlot],
      Object.freeze({ changeCorrelator: false })
    );
    gpt.emit('slotRequested', { slot: physicalSlot });
    gpt.emit('slotRenderEnded', {
      isEmpty: true,
      responseIdentifier: 'response-one',
      slot: physicalSlot,
    });
    await Promise.resolve();

    expect(primary.snapshot().outcome).toEqual({ outcome: 'failed', reason: 'gam_empty' });
    expect(fallback).toBeDefined();
    fallback?.fail('winner_not_renderable');
    expect(operation.ok && operation.value.snapshot()).toMatchObject({
      settled: true,
      result: {
        path: 'fallback',
        primary: { outcome: 'failed', reason: 'gam_empty' },
        fallback: { outcome: 'failed', reason: 'winner_not_renderable' },
      },
    });
    composition.runtime.dispose();
    slotElement.remove();
  });

  it('derives exact APS validation coordinates only for the real browser target', () => {
    const renderer = {
      type: 'aps',
      version: 1,
      accountId: 'publisher-account',
      bidId: 'bid-1',
      tagType: 'iframe',
      creativeUrl: 'https://creative.example/render',
      width: 300,
      height: 250,
      aaxResponse: 'renderer-envelope',
    };
    const message = {
      message: 'TS APS Start',
      version: 1,
      lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
      rendererUrl: new URL('/integrations/aps/renderer/v1', window.location.origin).href,
      envelope: {
        version: 1,
        nonce: 'n1_abcdefghijklmnopqrstuv',
        publisherOrigin: window.location.origin,
        renderer,
      },
    };
    const validation = { validateApsRenderer: () => true };

    const browser = createBrowserComposition({ messagingValidation: validation });
    expect(browser.adapters.messaging.parseProtocolMessage('apsStart', message)).toBeDefined();

    const injected = createBrowserComposition({
      target: createTarget(),
      messagingValidation: validation,
    });
    expect(injected.adapters.messaging.parseProtocolMessage('apsStart', message)).toBeUndefined();
  });

  it('installs the capture-phase message listener synchronously and disposes once', () => {
    const target = createTarget();
    const composition = createBrowserComposition({ target });
    const listener = vi.fn();

    const dispose = composition.adapters.messaging.installCaptureListener(listener);
    expect(dispose).toBeTypeOf('function');

    expect(target.addEventListener).toHaveBeenCalledTimes(1);
    const installed = target.addEventListener.mock.calls[0]?.[1];
    expect(installed).toBeTypeOf('function');
    expect(target.addEventListener).toHaveBeenCalledWith('message', installed, true);

    dispose?.();
    dispose?.();
    expect(target.removeEventListener).toHaveBeenCalledTimes(1);
    expect(target.removeEventListener).toHaveBeenCalledWith('message', installed, true);
  });

  it('uses exact injected fakes without constructing concrete adapters', () => {
    const googletag = fakeGoogletagAdapter(() => 'present');
    const prebid = fakePrebidAdapter(() => 'incompatible');
    const messaging = fakeMessagingAdapter();

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
    const disposeMessaging = composition.adapters.messaging.installCaptureListener(listener);
    expect(disposeMessaging).toBeUndefined();
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
          googletag: fakeGoogletagAdapter(),
          prebid: fakePrebidAdapter(),
          messaging: fakeMessagingAdapter(() => {
            order.push('bridge');
            return () => order.push('dispose-bridge');
          }),
        },
        coreActivations: {
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
    expect(composition.pucBridgeForTest()).toBeDefined();

    composition.runtime.dispose();
    expect(order).toEqual([
      'bridge',
      'gpt',
      'module',
      'dispose-module',
      'dispose-gpt',
      'dispose-bridge',
    ]);
    expect(composition.pucBridgeForTest()).toBeUndefined();
    expect(() => composition.adapters.googletag.run(() => undefined)).toThrowError(
      expect.objectContaining({ code: 'operation_disposed' })
    );
    expect(() => composition.adapters.prebid.run(() => undefined)).toThrowError(
      expect.objectContaining({ code: 'operation_disposed' })
    );
    expect(Object.isFrozen(composition)).toBe(true);
    expect(Object.isFrozen(composition.runtime)).toBe(true);
  });

  it('starts slot listeners before post-commit GPT startup and disposes both listeners', async () => {
    const releaseId = 'a'.repeat(64);
    const subscriptions: string[] = [];
    const releases: string[] = [];
    const facade = {
      bindingToken: () => Object.freeze({}),
      subscribe: (eventType: string) => {
        subscriptions.push(eventType);
        return () => releases.push(eventType);
      },
    } as unknown as GoogletagFacade;
    const googletag: GoogletagAdapter = Object.freeze({
      bindingStatus: () => 'present',
      dispose: vi.fn(),
      notifyReady: vi.fn(),
      observePublisherCalls: () => vi.fn(),
      run: <T>(command: (gpt: Readonly<GoogletagFacade>) => T) => {
        const result = Promise.resolve(command(facade));
        return Object.freeze({ status: 'present' as const, result, dispose: vi.fn() });
      },
    });
    const correctness = vi.fn(
      (
        _context: unknown,
        _adapters: unknown,
        services: { readonly slots: { readonly snapshotForTest: () => { records: number } } }
      ) => {
        expect(subscriptions).toEqual([]);
        expect(services.slots.snapshotForTest().records).toBe(0);
      }
    );
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: {
          version: 1,
          releaseId,
          integrations: [{ id: 'gpt', required: true }],
        },
        knownIntegrationIds: Object.freeze(['gpt']),
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
        adapters: {
          googletag,
          messaging: fakeMessagingAdapter(() => {
            expect(subscriptions).toEqual([]);
            return vi.fn();
          }),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: {
          correctnessGptListeners: correctness,
        },
        gptStartupForTest: () => {
          expect(subscriptions).toEqual(['slotRequested', 'slotRenderEnded']);
        },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    expect(
      composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
    ).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(correctness).toHaveBeenCalledOnce();
    composition.runtime.dispose();
    expect(releases).toEqual(['slotRenderEnded', 'slotRequested']);
  });

  it('injects GPT and Prebid module boundaries with only server-frozen configuration', async () => {
    const releaseId = 'a'.repeat(64);
    const target = {};
    const gptConfig = Object.freeze({ scriptUrl: '/integrations/gpt/script' });
    const prebidConfig = Object.freeze({ clientSideBidders: Object.freeze(['rubicon']) });
    const providedBindings = vi.fn((id: string) => ({
      config: id === 'prebid' ? prebidConfig : gptConfig,
      interfaces: Object.freeze({ publisherControlled: Object.freeze({}) }),
    }));
    const startGpt = vi.fn((received: unknown) => {
      expect(received).toBe(gptConfig);
      expect((target as { version?: unknown }).version).toBe('1.0.0');
    });
    const startPrebid = vi.fn((received: unknown) => {
      expect(received).toBe(prebidConfig);
      expect((target as { version?: unknown }).version).toBe('1.0.0');
    });
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest: {
          version: 1,
          releaseId,
          integrations: [
            { id: 'gpt', required: true },
            { id: 'prebid', required: true },
          ],
        },
        knownIntegrationIds: Object.freeze(['gpt', 'prebid']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: providedBindings,
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(() => vi.fn()),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        gptStartupForTest: startGpt,
        prebidStartupForTest: startPrebid,
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
      ).toBe(true);
      expect(
        composition.runtime.registerIntegration(createPrebidIntegrationRegistration(releaseId))
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });

      expect(providedBindings).toHaveBeenCalledTimes(2);
      expect(providedBindings).toHaveBeenNthCalledWith(1, 'gpt');
      expect(providedBindings).toHaveBeenNthCalledWith(2, 'prebid');
      expect(startGpt).toHaveBeenCalledExactlyOnceWith(gptConfig);
      expect(startPrebid).toHaveBeenCalledExactlyOnceWith(prebidConfig);
      expect(isGuardInstalled()).toBe(true);
      expect(composition.runtimeSessionForTest()?.interfaces).not.toHaveProperty(
        'publisherControlled'
      );
    } finally {
      composition.runtime.dispose();
      resetGuardState();
    }
    expect(isGuardInstalled()).toBe(false);
  });

  it('injects the exact creative boot into reversible activation and post-commit startup', async () => {
    const releaseId = 'a'.repeat(64);
    const creative = Object.freeze({
      version: 1 as const,
      enabled: true,
      clickGuard: true,
      renderGuard: false,
    });
    const release = vi.fn();
    const activateCreative = vi.fn((received: unknown) => {
      expect(received).toEqual(creative);
      expect(Object.isFrozen(received)).toBe(true);
      return release;
    });
    const startCreative = vi.fn((received: unknown) => {
      expect(received).toEqual(creative);
      expect(Object.isFrozen(received)).toBe(true);
    });
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: {
          version: 1,
          releaseId,
          integrations: [{ id: 'creative', required: true }],
        },
        knownIntegrationIds: Object.freeze(['creative']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            bids: [],
          },
          creative,
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        creativeActivationForTest: activateCreative,
        creativeStartupForTest: startCreative,
      }
    );

    expect(composition.runtime.start()).toBe(true);
    expect(
      composition.runtime.registerIntegration(createCreativeIntegrationRegistration(releaseId))
    ).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(activateCreative).toHaveBeenCalledTimes(1);
    expect(startCreative).toHaveBeenCalledTimes(1);
    expect(activateCreative.mock.calls[0]?.[0]).toBe(startCreative.mock.calls[0]?.[0]);

    composition.runtime.dispose();
    composition.runtime.dispose();
    expect(release).toHaveBeenCalledTimes(1);
  });

  it('owns the real creative click guard through the composition lifecycle', async () => {
    const releaseId = 'a'.repeat(64);
    const addEventListener = vi.spyOn(document, 'addEventListener');
    const removeEventListener = vi.spyOn(document, 'removeEventListener');
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: {
          version: 1,
          releaseId,
          integrations: [{ id: 'creative', required: true }],
        },
        knownIntegrationIds: Object.freeze(['creative']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            bids: [],
          },
          creative: { version: 1, enabled: true, clickGuard: true, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(createCreativeIntegrationRegistration(releaseId))
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      expect(addEventListener.mock.calls.filter(([type]) => type === 'click')).toHaveLength(1);
      expect(addEventListener.mock.calls.filter(([type]) => type === 'auxclick')).toHaveLength(1);
    } finally {
      composition.runtime.dispose();
      addEventListener.mockRestore();
    }
    expect(removeEventListener.mock.calls.filter(([type]) => type === 'click')).toHaveLength(1);
    expect(removeEventListener.mock.calls.filter(([type]) => type === 'auxclick')).toHaveLength(1);
    removeEventListener.mockRestore();
  });

  it('publishes and promotes one exact Prebid winner through runtime-owned PUC state', async () => {
    const releaseId = 'a'.repeat(64);
    const prebid = synchronousPrebidAdapter();
    const reservationId = `r1_${'p'.repeat(22)}`;
    let captureListener: CaptureMessageListener | undefined;
    const messagingTarget = {
      addEventListener: vi.fn(
        (_type: 'message', listener: CaptureMessageListener, _capture: true) => {
          captureListener = listener;
        }
      ),
      removeEventListener: vi.fn(),
    };
    const messaging = createBrowserMessagingAdapter(messagingTarget);
    const bid = Object.freeze({
      candidateId: 'AAAAAAAAAAAA',
      slot: 'slot-one',
      provider: 'trusted',
      upstreamBidId: 'upstream-one',
      cpm: 1.25,
      currency: 'USD' as const,
      targeting: Object.freeze({ hb_bidder: 'trustedServer' }),
      rendererReservationId: reservationId,
      renderSource: Object.freeze({
        type: 'adm' as const,
        version: 1 as const,
        adm: '<main>private creative</main>',
        width: 300,
        height: 250,
      }),
    });
    const projection = Object.freeze({
      version: 1,
      auction: Object.freeze({
        version: 1,
        auctionId: 'auction-one',
        results: Object.freeze([
          Object.freeze({
            slot: bid.slot,
            outcome: 'winner' as const,
            candidateId: bid.candidateId,
          }),
        ]),
      }),
      bids: Object.freeze([bid]),
    });
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: {
          version: 1,
          releaseId,
          integrations: [{ id: 'prebid', required: true }],
        },
        knownIntegrationIds: Object.freeze(['prebid']),
        boot: {
          auctionProjection: projection,
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging,
          prebid: prebid.adapter,
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        createIdentityIssuerForTest: () =>
          createTestNavigationIdentityIssuer({
            getRandomValues: (target) => {
              target.fill(9);
              return target;
            },
          }),
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(createPrebidIntegrationRegistration(releaseId))
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      const complete = vi.fn();
      prebid.auction(
        Object.freeze({
          auctionId: 'auction-one',
          bids: Object.freeze([Object.freeze({ adUnitCode: bid.slot, requestId: 'request-one' })]),
          complete,
        })
      );

      expect(complete).toHaveBeenCalledTimes(1);
      expect(prebid.admitTrustedBid).toHaveBeenCalledTimes(1);
      expect(prebid.admitTrustedBid.mock.calls[0]?.[0]).toMatchObject({
        auctionId: 'auction-one',
        adUnitCode: bid.slot,
        bid: { adId: reservationId, requestId: 'request-one' },
      });
      expect(composition.reservationServiceForTest()?.recognize(reservationId)).toMatchObject({
        state: 'awaiting_prebid_selection',
      });

      prebid.auctionEnd('auction-one');
      expect(composition.reservationServiceForTest()?.recognize(reservationId)).toMatchObject({
        state: 'renderable',
      });
      expect(composition.pucBridgeForTest()?.snapshotInventoryForTest()).toMatchObject({
        attempts: 1,
      });

      const claimPort = {
        addEventListener: vi.fn(),
        close: vi.fn(),
        postMessage: vi.fn(),
        removeEventListener: vi.fn(),
        start: vi.fn(),
      };
      captureListener?.({
        data: JSON.stringify({
          message: 'Prebid Request',
          adId: reservationId,
          adServerDomain: 'ads.example.com',
        }),
        ports: [claimPort],
        source: Object.freeze({ frame: 'selected-creative' }),
        stopImmediatePropagation: vi.fn(),
      } as unknown as MessageEvent);
      expect(composition.pucBridgeForTest()?.snapshotInventoryForTest()).toMatchObject({
        attempts: 1,
        liveTickets: 0,
        pendingClaims: 1,
      });
      expect(claimPort.postMessage).not.toHaveBeenCalled();
      expect(claimPort.close).not.toHaveBeenCalled();
    } finally {
      composition.runtime.dispose();
    }
  });

  it('hands late publisher GPT calls through the adapter into runtime-owned slot state', async () => {
    const releaseId = 'a'.repeat(64);
    const slot = Object.freeze({ id: 'trusted-slot' });
    const unrelated = Object.freeze({ id: 'publisher-slot' });
    const refresh = vi.fn((_slots?: readonly object[], _options?: unknown) => undefined);
    const display = vi.fn((_target: unknown) => undefined);
    const destroySlots = vi.fn((_slots?: readonly object[]) => true);
    const listeners = new Map<string, Set<(event: unknown) => void>>();
    const pubads = {
      addEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
        const registered = listeners.get(type) ?? new Set();
        registered.add(listener);
        listeners.set(type, registered);
      }),
      disableInitialLoad: vi.fn(),
      getSlots: vi.fn(() => [slot, unrelated]),
      refresh,
      removeEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
        listeners.get(type)?.delete(listener);
      }),
    };
    const nativeDefineSlot = vi.fn((_path: string, _sizes: unknown, _elementId: string) =>
      Object.freeze({ id: 'duplicate' })
    );
    const googletag = {
      apiReady: true,
      pubadsReady: true,
      cmd: { push: (command: () => void) => (command(), 0) },
      defineSlot: nativeDefineSlot,
      destroySlots,
      display,
      getConfig: vi.fn(() => ({ disableInitialLoad: true })),
      pubads: () => pubads,
      setConfig: vi.fn(),
    };
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: { version: 1, releaseId, integrations: [{ id: 'gpt', required: true }] },
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: {
              version: 1,
              auctionId: 'initial',
              results: [{ slot: 'slot', outcome: 'no_bid' }],
            },
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: createBrowserGoogletagAdapter({ googletag }),
          messaging: fakeMessagingAdapter(() => vi.fn()),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      const navigation = composition.runtimeSessionForTest()?.currentNavigation;
      const slots = composition.slotServiceForTest();
      if (!navigation || !slots) throw new Error('Expected active GPT composition');
      expect(
        slots.adoptGptSlot(navigation.generation, 'slot', {
          definition: {
            adUnitPath: '/trusted/path',
            elementId: 'slot-div',
            sizes: Object.freeze([[300, 250]]),
          },
          elementIdPrefix: 'slot-',
          ownership: 'trusted_server',
          slot,
        })
      ).toEqual({ ok: true });

      expect(googletag.defineSlot('/publisher/mismatch', [728, 90], 'slot-div')).toBe(slot);
      expect(nativeDefineSlot).not.toHaveBeenCalled();
      expect(googletag.display('slot-div')).toBeUndefined();
      expect(display).not.toHaveBeenCalled();
      const options = Object.freeze({ changeCorrelator: true, publisher: 'preserved' });
      expect(pubads.refresh(undefined, options)).toBeUndefined();
      expect(refresh).toHaveBeenCalledExactlyOnceWith([unrelated], options);
      pubads.refresh([slot], options);
      expect(refresh).toHaveBeenLastCalledWith([slot], options);
      const request = slots.request({
        intentId: 'publisher-owned',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'slot',
        requestClass: 'primary',
      });
      await expect(request.result).resolves.toEqual({
        status: 'failed',
        reason: 'cycle_unattributable',
      });
      expect(googletag.destroySlots([slot])).toBe(true);
      expect(slots.isBoundGptSlot(navigation.generation, 'slot', slot)).toBe(false);
    } finally {
      composition.runtime.dispose();
      resetGuardState();
    }
    expect(destroySlots).toHaveBeenCalledTimes(1);
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
          googletag: fakeGoogletagAdapter(),
          prebid: fakePrebidAdapter(),
          messaging: fakeMessagingAdapter(),
        },
        coreActivations: {
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
    const slotService = composition.slotServiceForTest();
    const targetingService = composition.targetingServiceForTest();
    const reservationService = composition.reservationServiceForTest();
    const rendererNonces = composition.rendererNonceRegistryForTest();
    expect(slotService).toBeDefined();
    expect(targetingService).toBeDefined();
    expect(reservationService).toBeDefined();
    expect(rendererNonces).toBeDefined();
    expect(session?.interfaces['slots']).toBe(slotService);
    expect(session?.interfaces['targeting']).toBe(targetingService);
    expect(session?.interfaces['reservations']).toBe(reservationService);
    expect(session?.interfaces['rendererNonces']).toBe(rendererNonces);
    expect(session?.interfaces['renderDirectAps']).toBeTypeOf('function');
    expect(session?.interfaces['renderDirectAdm']).toBeTypeOf('function');
    expect(session?.interfaces['renderDirectCache']).toBeTypeOf('function');
    expect(session?.currentNavigation?.interfaces).toBe(session?.interfaces);
    expect(session?.currentNavigation?.currentAuctionProjection).toEqual(projection);
    expect(Object.isFrozen(session?.currentNavigation?.currentAuctionProjection)).toBe(true);

    const initialNavigation = session?.currentNavigation;
    const artifactBatch = initialNavigation?.createAuctionBatch('accepted-artifact');
    const artifactOwner = artifactBatch?.createRenderAttempt('accepted-artifact-slot');
    const artifactStore = session?.interfaces['artifacts'] as
      Parameters<typeof createRenderAttempt>[0]['artifacts'] | undefined;
    if (!artifactOwner?.ok || !artifactStore || !reservationService) {
      throw new Error('Expected accepted-artifact dependencies');
    }
    const acceptedSource = Object.freeze({
      type: 'adm' as const,
      version: 1 as const,
      adm: '<main>accepted</main>',
      width: 300,
      height: 250,
    });
    const acceptedAttempt = createRenderAttempt({
      artifacts: artifactStore,
      owner: artifactOwner.value,
      prepareRenderSource: () => acceptedSource,
      reservations: reservationService,
    });
    if (!acceptedAttempt.ok) throw new Error(acceptedAttempt.reason);
    const disposeAcceptedArtifact = vi.fn();
    const acceptedArtifact = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: acceptedAttempt.value.id,
      slot: acceptedAttempt.value.slot,
      navigationGeneration: acceptedAttempt.value.navigationGeneration,
      dispose: disposeAcceptedArtifact,
    });
    expect(
      acceptedAttempt.value.admitDirectWinner(acceptedSource, Object.freeze({ selectedCpm: 1 }))
    ).toBe(true);
    expect(acceptedAttempt.value.beginDirect()).toBe(true);
    expect(acceptedAttempt.value.beginAdm(acceptedArtifact)).toBe(true);
    expect(acceptedAttempt.value.accept()).toBe(true);
    expect(artifactStore.current('accepted-artifact-slot')).toBe(acceptedArtifact);

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
    expect(disposeAcceptedArtifact).toHaveBeenCalledOnce();
    expect(artifactStore.current('accepted-artifact-slot')).toBeUndefined();
    expect(replacement.value.currentAuctionProjection).toBeUndefined();
    expect(composition.runtimeSessionForTest()).toBe(session);
    expect(composition.reservationServiceForTest()).toBe(reservationService);

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
    expect(slotService?.snapshotForTest()).toEqual({
      cycles: 0,
      intents: 0,
      physicalSlots: 0,
      records: 0,
    });
    expect(targetingService?.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
    expect(reservationService?.snapshotInventoryForTest()).toMatchObject({
      disposed: true,
      size: 0,
    });
    expect(rendererNonces?.snapshotForTest()).toMatchObject({ disposed: true });
    expect(composition.slotServiceForTest()).toBeUndefined();
    expect(composition.targetingServiceForTest()).toBeUndefined();
    expect(composition.reservationServiceForTest()).toBeUndefined();
    expect(composition.rendererNonceRegistryForTest()).toBeUndefined();
  });

  it('unwinds a lazily-created session when navigation identity generation fails', async () => {
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
    expect(composition.pucBridgeForTest()).toBeUndefined();
  });

  it('falls back before publishing services when the PUC capture listener cannot install', async () => {
    const correctnessGptListeners = vi.fn();
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
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(() => undefined),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(correctnessGptListeners).not.toHaveBeenCalled();
    expect(composition.pucBridgeForTest()).toBeUndefined();
    expect(composition.slotServiceForTest()).toBeUndefined();
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

  it.each([
    [2, 255],
    [1, 256],
  ] as const)(
    'rejects one atomic initial registration of %i server plus %i programmatic records',
    async (serverCount, programmaticCount) => {
      const composition = createTestBrowserRuntimeComposition(
        {
          target: {},
          releaseId: 'a'.repeat(64),
          manifest: { version: 1, releaseId: 'a'.repeat(64), integrations: [] },
          knownIntegrationIds: Object.freeze([]),
          boot: {
            auctionProjection: {
              version: 1,
              auction: {
                version: 1,
                auctionId: 'initial',
                results: Array.from({ length: serverCount }, (_, index) => ({
                  outcome: 'no_bid' as const,
                  slot: `server-${index}`,
                })),
              },
              bids: [],
            },
            creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
            diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
          },
          kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
        },
        {
          admittedProgrammaticSlotsForTest: Object.freeze(
            Array.from({ length: programmaticCount }, (_, index) => `programmatic-${index}`)
          ),
          coreActivations: {
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
      expect(composition.slotServiceForTest()).toBeUndefined();
      expect(composition.projectionSlotsForTest()).toBeUndefined();
    }
  );

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

  it('fails an admitted cache attempt when owner activation captured no fetch authority', async () => {
    const fetchDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'fetch');
    Object.defineProperty(globalThis, 'fetch', {
      configurable: true,
      value: undefined,
      writable: true,
    });
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
          cachePolicy: {
            version: 1,
            baseUrl: 'https://cache.example/pbc/v1/cache',
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: {
          correctnessGptListeners: vi.fn(),
        },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      const renderCache = composition.runtimeSessionForTest()?.interfaces['renderDirectCache'] as
        ((attempt: RenderAttempt, container: HTMLElement) => boolean) | undefined;
      const fail = vi.fn(() => true);
      expect(renderCache).toBeTypeOf('function');
      expect(
        renderCache?.(Object.freeze({ fail }) as unknown as RenderAttempt, document.body)
      ).toBe(false);
      expect(fail).toHaveBeenCalledOnce();
      expect(fail).toHaveBeenCalledWith('cache_network_error');
    } finally {
      composition.runtime.dispose();
      if (fetchDescriptor) Object.defineProperty(globalThis, 'fetch', fetchDescriptor);
      else Reflect.deleteProperty(globalThis, 'fetch');
    }
  });

  it('constructs or activates nothing after a terminal fallback', async () => {
    vi.useFakeTimers();
    const serviceConstruction = vi.fn(() => ({
      config: Object.freeze({}),
      interfaces: Object.freeze({}),
    }));
    const adapterActivation = vi.fn(() => 'pending' as const);
    const listenerActivation = vi.fn(() => vi.fn());
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
          googletag: fakeGoogletagAdapter(adapterActivation),
          prebid: fakePrebidAdapter(adapterActivation),
          messaging: fakeMessagingAdapter(listenerActivation),
        },
        coreActivations: {
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
    expect(latePreparation).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('isolates and locally logs a throwing auction-context contributor', async () => {
    const releaseId = 'a'.repeat(64);
    const target = {};
    const requestConfigs: unknown[] = [];
    const auctionFetcher = vi.fn(async (_input: string, init: RequestInit) => {
      const body = JSON.parse(String(init.body)) as { config: unknown };
      requestConfigs.push(body.config);
      return {
        ok: true,
        json: async () => ({
          id: 'context-auction',
          cur: 'USD',
          seatbid: [],
          ext: {
            trusted_server: {
              slot_results: {
                version: 1,
                auctionId: 'context-auction',
                results: [{ slot: 'server-slot', outcome: 'no_bid' }],
              },
            },
          },
        }),
      };
    });
    const warn = vi.spyOn(localLog, 'warn').mockImplementation(() => undefined);
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest: {
          version: 1,
          releaseId,
          integrations: [{ id: 'context_test', required: true }],
        },
        knownIntegrationIds: Object.freeze(['context_test']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: {
              version: 1,
              auctionId: 'initial',
              results: [{ slot: 'server-slot', outcome: 'no_bid' }],
            },
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        auctionFetcherForTest: auctionFetcher,
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration({
          id: 'context_test',
          release: releaseId,
          prepare: () => ({ activate: vi.fn() }),
        })
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      expect((target as { log?: unknown }).log).toBe(publicLog);

      const registry = composition.auctionContextRegistryForTest();
      const session = composition.runtimeSessionForTest();
      expect(registry).toBeDefined();
      expect(session).toBeDefined();
      expect(
        registry?.register(
          'context_test',
          () => {
            throw new Error('publisher contributor');
          },
          session!
        )
      ).toBe(true);

      const api = target as {
        requestAds(options?: unknown): Promise<{ readonly slots: readonly object[] }>;
      };
      await expect(api.requestAds({ slots: ['server-slot'] })).resolves.toEqual({
        slots: [{ slot: 'server-slot', path: 'primary', outcome: 'no_bid' }],
      });

      expect(requestConfigs).toEqual([{}]);
      expect(warn).toHaveBeenCalledExactlyOnceWith('auction context: contributor failed', {
        integrationId: 'context_test',
        reason: 'contributor_failed',
      });
    } finally {
      composition.runtime.dispose();
      warn.mockRestore();
    }
  });

  it('exercises transactional addAdUnits and invocation-time requestAds snapshots through the test kernel', async () => {
    const target = {};
    const requestBodies: Array<{
      adUnits: Array<{ code: string }>;
      config: Readonly<Record<string, unknown>>;
    }> = [];
    const auctionFetcher = vi.fn(async (_input: string, init: RequestInit) => {
      const body = JSON.parse(String(init.body)) as {
        adUnits: Array<{ code: string }>;
        config: Readonly<Record<string, unknown>>;
      };
      requestBodies.push(body);
      const slots = body.adUnits.map(({ code }) => code);
      const winnerSlot =
        requestBodies.length === 1 || (slots.length === 1 && slots[0] === 'ambiguous-slot')
          ? slots[0]
          : undefined;
      const candidateId = 'AAAAAAAAAAAA';
      const renderSource = {
        type: 'adm',
        version: 1,
        adm: '<div>programmatic winner</div>',
        width: 300,
        height: 250,
      } as const;
      return {
        ok: true,
        json: async () => ({
          id: `auction-${requestBodies.length}`,
          cur: 'USD',
          seatbid: winnerSlot
            ? [
                {
                  seat: 'fictional',
                  bid: [
                    {
                      id: 'r1_AAAAAAAAAAAAAAAAAAAAAA',
                      impid: winnerSlot,
                      price: 1,
                      adm: renderSource.adm,
                      w: renderSource.width,
                      h: renderSource.height,
                      ext: {
                        trusted_server: {
                          candidate_id: candidateId,
                          slot_id: winnerSlot,
                          render_source: renderSource,
                        },
                      },
                    },
                  ],
                },
              ]
            : [],
          ext: {
            trusted_server: {
              slot_results: {
                version: 1,
                auctionId: `auction-${requestBodies.length}`,
                results: slots.map((slot) =>
                  slot === winnerSlot
                    ? { slot, outcome: 'winner', candidateId }
                    : { slot, outcome: 'no_bid' }
                ),
              },
            },
          },
        }),
      };
    });
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId: 'a'.repeat(64),
        manifest: {
          version: 1,
          releaseId: 'a'.repeat(64),
          integrations: [{ id: 'context_test', required: true }],
        },
        knownIntegrationIds: Object.freeze(['context_test']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: {
              version: 1,
              auctionId: 'initial',
              results: [{ slot: 'server-slot', outcome: 'no_bid' }],
            },
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        auctionFetcherForTest: auctionFetcher,
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    expect(
      composition.runtime.registerIntegration({
        id: 'context_test',
        release: 'a'.repeat(64),
        prepare: () => ({ activate: vi.fn() }),
      })
    ).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    const contextContributor = vi.fn(() => ({ page: 'context' }));
    const session = composition.runtimeSessionForTest();
    expect(session).toBeDefined();
    expect(
      composition
        .auctionContextRegistryForTest()
        ?.register('context_test', contextContributor, session!)
    ).toBe(true);
    const api = target as {
      addAdUnits(value: unknown): { readonly registered: readonly string[] };
      requestAds(options?: unknown): Promise<{ readonly slots: readonly object[] }>;
    };
    const programmatic = {
      code: 'programmatic-slot',
      mediaTypes: { banner: { sizes: [[300, 250]] } },
      bids: [{ bidder: 'fictional', params: { placement: 7 } }],
    };

    expect(api.addAdUnits(programmatic)).toEqual({ registered: ['programmatic-slot'] });
    expect(composition.projectionSlotsForTest()).toEqual(['server-slot', 'programmatic-slot']);
    const slotService = composition.slotServiceForTest();
    expect(slotService?.resolveRegisteredSlot('programmatic-slot')).toMatchObject({
      domAliases: [],
      registeredSlotId: 'programmatic-slot',
      source: 'programmatic',
    });
    expect(slotService?.resolveDomAlias('programmatic-slot')).toBeUndefined();
    expect(() =>
      api.addAdUnits([
        {
          code: 'must-roll-back',
          mediaTypes: { banner: { sizes: [[1, 1]] } },
        },
        {
          code: 'server-slot',
          mediaTypes: { banner: { sizes: [[1, 1]] } },
        },
      ])
    ).toThrowError(expect.objectContaining({ code: 'slot_collision', unitIndex: 1 }));
    expect(composition.projectionSlotsForTest()).toEqual(['server-slot', 'programmatic-slot']);
    document.body.innerHTML = '<div id="programmatic-slot"><span>placeholder</span></div>';
    const explicit = api.requestAds({ slots: ['unknown', 'programmatic-slot'] });
    await vi.waitFor(() =>
      expect(document.querySelector('#programmatic-slot iframe')).not.toBeNull()
    );
    const frame = document.querySelector<HTMLIFrameElement>('#programmatic-slot iframe');
    expect(frame?.srcdoc).toContain('programmatic winner');
    frame?.dispatchEvent(new Event('load'));
    await expect(explicit).resolves.toEqual({
      slots: [
        { slot: 'unknown', path: 'primary', outcome: 'failed', reason: 'slot_unresolved' },
        { slot: 'programmatic-slot', path: 'primary', outcome: 'accepted' },
      ],
    });
    expect(requestBodies[0]).toEqual({
      adUnits: [programmatic],
      config: { page: 'context' },
    });
    expect(contextContributor).toHaveBeenCalledOnce();

    const omitted = api.requestAds();
    expect(
      api.addAdUnits({
        code: 'later-slot',
        mediaTypes: { banner: { sizes: [[728, 90]] } },
      })
    ).toEqual({ registered: ['later-slot'] });
    await expect(omitted).resolves.toEqual({
      slots: [
        { slot: 'server-slot', path: 'primary', outcome: 'no_bid' },
        { slot: 'programmatic-slot', path: 'primary', outcome: 'no_bid' },
      ],
    });
    expect(requestBodies[1]?.adUnits.map(({ code }) => code)).toEqual([
      'server-slot',
      'programmatic-slot',
    ]);
    expect(requestBodies[1]?.adUnits).not.toContainEqual(
      expect.objectContaining({ code: 'later-slot' })
    );
    expect(requestBodies[1]?.config).toEqual({ page: 'context' });
    expect(contextContributor).toHaveBeenCalledTimes(2);
    expect(auctionFetcher).toHaveBeenCalledTimes(2);

    expect(
      slotService?.register(session!.currentNavigation!, [
        {
          adUnitCode: '/network/path',
          domAliases: ['publisher-alias'],
          registeredSlotId: 'alias-owner',
          source: 'server',
        },
      ])
    ).toMatchObject({ ok: true });
    await expect(
      api.requestAds({ slots: ['publisher-alias', '/network/path', 'alias-owner'] })
    ).resolves.toEqual({
      slots: [
        { slot: 'publisher-alias', path: 'primary', outcome: 'failed', reason: 'slot_unresolved' },
        { slot: '/network/path', path: 'primary', outcome: 'failed', reason: 'slot_unresolved' },
        { slot: 'alias-owner', path: 'primary', outcome: 'no_bid' },
      ],
    });
    expect(requestBodies[2]?.adUnits.map(({ code }) => code)).toEqual(['alias-owner']);

    expect(
      api.addAdUnits({
        code: 'ambiguous-slot',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      })
    ).toEqual({ registered: ['ambiguous-slot'] });
    document.body.insertAdjacentHTML(
      'beforeend',
      '<div id="ambiguous-slot"></div><div id="ambiguous-slot"></div>'
    );
    await expect(api.requestAds({ slots: ['ambiguous-slot'] })).resolves.toEqual({
      slots: [
        {
          slot: 'ambiguous-slot',
          path: 'primary',
          outcome: 'failed',
          reason: 'slot_unresolved',
        },
      ],
    });
    expect(document.querySelectorAll('[id="ambiguous-slot"] iframe')).toHaveLength(0);
    expect(contextContributor).toHaveBeenCalledTimes(4);
    expect(auctionFetcher).toHaveBeenCalledTimes(4);

    composition.runtime.dispose();
    expect(() => api.addAdUnits(programmatic)).toThrowError(
      expect.objectContaining({ name: 'AdUnitRegistrationError', code: 'slot_collision' })
    );
    document.body.innerHTML = '';
  });
});
