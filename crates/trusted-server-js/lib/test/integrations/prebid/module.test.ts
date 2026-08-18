import { afterEach, describe, expect, it, vi } from 'vitest';

import type { GoogletagAdapter } from '../../../src/adapters/googletag';
import {
  PrebidAdmissionContractError,
  type PrebidAdapter,
  type PrebidFacade,
} from '../../../src/adapters/prebid';
import {
  createPrebidIntegrationRegistration,
  createPrebidSelectionCoordinator,
  publishPrebidBid,
  type PrebidBidPublicationInput,
  type PreparedTrustedBidV1,
} from '../../../src/integrations/prebid/module';
import {
  createPrebidRefreshPolicy,
  createPrebidSyntheticRefreshRunner,
  preparePrebidRegisteredRefreshAuction,
} from '../../../src/integrations/prebid/refresh';
import { createTestNavigationIdentityIssuer } from '../../../src/kernel/identity';
import {
  createIntegrationRegistry,
  type IntegrationActivationContext,
  type IntegrationInstallCallbacks,
  type IntegrationRegistration,
} from '../../../src/kernel/integration_registry';
import { createRuntimeSession, type RenderAttemptScope } from '../../../src/kernel/sessions';
import {
  createCommittedArtifactStore,
  createRenderAttempt,
  type RenderAttempt,
} from '../../../src/services/render';
import { createReservationService } from '../../../src/services/reservations';

const RELEASE_ID = 'a'.repeat(64);

function manifest(ids: readonly string[]) {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    firstDisplay: null,
    runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
    integrations: ids.map((id) => ({ id, phase: 'takeover' as const })),
  };
}

function registration(
  id: string,
  prepare: IntegrationRegistration['prepare']
): IntegrationRegistration {
  return Object.freeze({ abi: 1, id, phase: 'takeover', releaseId: RELEASE_ID, prepare });
}

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

function recursivelyFrozen(candidate: unknown, seen = new Set<object>()): boolean {
  if (candidate === null || (typeof candidate !== 'object' && typeof candidate !== 'function')) {
    return typeof candidate !== 'number' || Number.isFinite(candidate);
  }
  if (typeof candidate === 'function' || seen.has(candidate) || !Object.isFrozen(candidate)) {
    return false;
  }
  const prototype = Object.getPrototypeOf(candidate);
  if (
    prototype !== Object.prototype &&
    prototype !== null &&
    !(Array.isArray(candidate) && prototype === Array.prototype)
  ) {
    return false;
  }
  seen.add(candidate);
  return Reflect.ownKeys(candidate).every((key) => {
    const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
    return (
      descriptor !== undefined && 'value' in descriptor && recursivelyFrozen(descriptor.value, seen)
    );
  });
}

function createLegacyPrebidRegistrationForTest(_releaseId: string): IntegrationRegistration {
  return registration('prebid', ({ config, interfaces }) => {
    if (!recursivelyFrozen(config)) throw new TypeError('Prebid test config is invalid');
    const runtime = interfaces['prebid'] as
      Readonly<{ activate?: () => unknown; start?: (config: unknown) => void }> | undefined;
    if (
      !runtime ||
      !Object.isFrozen(runtime) ||
      typeof runtime.activate !== 'function' ||
      typeof runtime.start !== 'function'
    ) {
      throw new TypeError('Prebid test runtime is unavailable');
    }
    const activate = runtime.activate;
    const start = runtime.start;
    return Object.freeze({
      activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
        const release = activate();
        if (typeof release !== 'function') {
          throw new TypeError('Prebid test runtime disposer is unavailable');
        }
        onDispose(release as () => void);
        afterCommit(() => start(config));
      },
    });
  });
}

type TrustedServerBidder = Readonly<{
  callBids: (
    request: Readonly<object>,
    addBidResponse: (adUnitCode: string, bid: Readonly<Record<string, unknown>>) => void,
    done: () => void
  ) => void;
}>;

function recursivelyFreeze<T>(value: T): T {
  if (value && typeof value === 'object' && !Object.isFrozen(value)) {
    for (const child of Object.values(value)) recursivelyFreeze(child);
    Object.freeze(value);
  }
  return value;
}

function productionPrebidBinding(userIdModules: readonly object[]) {
  const listeners = new Map<string, Set<(event: unknown) => void>>();
  const responses = new Map<string, readonly Readonly<Record<string, unknown>>[]>();
  let bidder: TrustedServerBidder | undefined;
  let highest: readonly object[] = Object.freeze([]);
  const responseFor = (adUnitCode: string) => {
    const response = [...(responses.get(adUnitCode) ?? [])] as object[] & { bids: object[] };
    response.bids = response;
    return response;
  };
  const pbjs = {
    addAdUnits: vi.fn(),
    getBidResponsesForAdUnitCode: vi.fn((adUnitCode: string) => responseFor(adUnitCode)),
    getHighestCpmBids: vi.fn(() => [...highest]),
    offEvent: vi.fn((type: string, listener: (event: unknown) => void) => {
      listeners.get(type)?.delete(listener);
    }),
    onEvent: vi.fn((type: string, listener: (event: unknown) => void) => {
      const current = listeners.get(type) ?? new Set();
      current.add(listener);
      listeners.set(type, current);
    }),
    processQueue: vi.fn(),
    registerBidAdapter: vi.fn((factory: () => TrustedServerBidder) => {
      bidder = factory();
    }),
    renderAd: vi.fn(),
    requestBids: vi.fn(),
    setTargetingForGPTAsync: vi.fn(),
    que: Object.freeze({
      push: (command: () => void) => {
        command();
        return 1;
      },
    }),
  };
  const stamp = recursivelyFreeze({
    abi: 1,
    artifactReleaseId: 'b'.repeat(64),
    prebidVersion: '10.26.0',
    moduleStems: ['alphaBidAdapter', 'sharedIdSystem'],
    bidderCodes: ['alpha'],
    bidderAliases: [],
    userIdModules: [...userIdModules],
  });
  Object.defineProperty(pbjs, '__trustedServerArtifactV1', {
    configurable: false,
    enumerable: false,
    value: stamp,
    writable: false,
  });
  return Object.freeze({
    addResponse: (adUnitCode: string, bid: Readonly<Record<string, unknown>>): void => {
      responses.set(adUnitCode, Object.freeze([bid]));
      for (const listener of listeners.get('bidResponse') ?? []) listener(bid);
    },
    bidder: () => bidder,
    emit: (type: string, event: unknown): void => {
      for (const listener of listeners.get(type) ?? []) listener(event);
    },
    pbjs,
    select: (bids: readonly object[]): void => {
      highest = Object.freeze([...bids]);
    },
  });
}

function requiredUserIdConfig() {
  return Object.freeze({
    clientSideBidders: Object.freeze(['alpha']),
    requiredUserIdModules: Object.freeze([
      Object.freeze({
        moduleName: 'sharedIdSystem',
        configNames: Object.freeze(['sharedId']),
        eidSources: Object.freeze(['sharedid.org']),
      }),
    ]),
  });
}

function initialProductionPrebidHarness(userIdModules: readonly object[]) {
  const binding = productionPrebidBinding(userIdModules);
  (window as unknown as { pbjs?: unknown }).pbjs = binding.pbjs;
  const runtime = createRuntimeSession({
    createIdentityIssuer: () =>
      createTestNavigationIdentityIssuer({
        getRandomValues: (target) => {
          target.fill(9);
          return target;
        },
      }),
  });
  const navigationResult = runtime.startInitialNavigation();
  if (!navigationResult.ok) throw new Error('Expected initial navigation');
  const navigation = navigationResult.value;
  const renderSource = Object.freeze({
    type: 'adm' as const,
    version: 1 as const,
    adm: '<main>production-prebid</main>',
    width: 300,
    height: 250,
  });
  const bid = Object.freeze({
    candidateId: 'AAAAAAAAAAAA',
    slot: 'slot-one',
    provider: 'aps',
    upstreamBidId: 'upstream-one',
    cpm: 1.25,
    currency: 'USD' as const,
    targeting: Object.freeze({ hb_bidder: 'trustedServer' }),
    rendererReservationId: `r1_${'p'.repeat(22)}`,
    renderSource,
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
  if (!navigation.installAuctionProjection(projection)) throw new Error('Expected projection');
  const reservations = createReservationService({
    prepareRenderSource: (candidate) =>
      typeof candidate === 'object' && candidate !== null && Object.isFrozen(candidate)
        ? (candidate as typeof renderSource)
        : undefined,
  });
  const artifacts = createCommittedArtifactStore();
  const registerPucGamAttempt = vi.fn(() => true);
  const createAttempt = (owner: RenderAttemptScope) =>
    createRenderAttempt({
      artifacts,
      owner,
      prepareRenderSource: (candidate) =>
        typeof candidate === 'object' && candidate !== null && Object.isFrozen(candidate)
          ? (candidate as typeof renderSource)
          : undefined,
      reservations,
    });
  const preparationDisposers: Array<() => void> = [];
  const activationDisposers: Array<() => void> = [];
  const afterCommit: Array<() => void> = [];
  const controller = new AbortController();
  return Object.freeze({
    activationContext: Object.freeze({
      afterCommit: (callback: () => void) => afterCommit.push(callback),
      onDispose: (callback: () => void) => activationDisposers.push(callback),
      signal: controller.signal,
    }),
    afterCommit,
    bid,
    binding,
    config: requiredUserIdConfig(),
    dispose: () => {
      for (let index = activationDisposers.length - 1; index >= 0; index -= 1) {
        activationDisposers[index]?.();
      }
      for (let index = preparationDisposers.length - 1; index >= 0; index -= 1) {
        preparationDisposers[index]?.();
      }
      reservations.dispose();
      artifacts.dispose();
      runtime.dispose();
      delete (window as unknown as { pbjs?: unknown }).pbjs;
    },
    interfaces: Object.freeze({
      'runtime.v1': Object.freeze({ document }),
      'slots.v1': Object.freeze({}),
      'render.v1': Object.freeze({
        createAttempt,
        navigation,
        projection,
        registerPucGamAttempt,
        reservations,
      }),
      'messages.v1': Object.freeze({}),
      'aps.v1': Object.freeze({}),
    }),
    navigation,
    prepareContext: Object.freeze({
      config: requiredUserIdConfig(),
      interfaces: Object.freeze({
        'runtime.v1': Object.freeze({ document }),
        'slots.v1': Object.freeze({}),
        'render.v1': Object.freeze({
          createAttempt,
          navigation,
          projection,
          registerPucGamAttempt,
          reservations,
        }),
        'messages.v1': Object.freeze({}),
        'aps.v1': Object.freeze({}),
      }),
      onDispose: (callback: () => void) => preparationDisposers.push(callback),
      signal: controller.signal,
    }),
    registerPucGamAttempt,
    reservations,
  });
}

describe('production Prebid takeover registration', () => {
  afterEach(() => {
    delete (window as unknown as { pbjs?: unknown }).pbjs;
  });

  it('passes exact configured user-ID/EID requirements into artifact admission', async () => {
    const harness = initialProductionPrebidHarness([]);
    try {
      const prepared = await createPrebidIntegrationRegistration(RELEASE_ID).prepare(
        harness.prepareContext
      );
      const capability = prepared.interfaces?.['prebid.v1'] as
        Readonly<{ adapter?: PrebidAdapter }> | undefined;
      expect(capability?.adapter?.bindingStatus()).toBe('incompatible');
    } finally {
      harness.dispose();
    }
  });

  it('adopts the sealed initial Prebid slice without replaying an initial bid publication', async () => {
    const harness = initialProductionPrebidHarness([
      Object.freeze({
        moduleName: 'sharedIdSystem',
        configNames: Object.freeze(['sharedId']),
        eidSources: Object.freeze(['sharedid.org']),
      }),
    ]);
    try {
      const prepared = await createPrebidIntegrationRegistration(RELEASE_ID).prepare(
        harness.prepareContext
      );
      const adoption = Object.freeze({
        version: 1 as const,
        adoptInitialDisplay: true as const,
        handoff: Object.freeze({
          slices: Object.freeze(['first_display', 'prebid_initial']),
          parserState: Object.freeze([
            Object.freeze({
              sliceId: 'prebid_initial',
              observations: Object.freeze(['protocol_version']),
              values: Object.freeze([Object.freeze(['protocol_version', 1] as const)]),
            }),
          ]),
        }),
        identities: Object.freeze([]),
      });

      prepared.activate(Object.freeze({ ...harness.activationContext, adoption }));
      for (const callback of harness.afterCommit) callback();

      expect(harness.binding.bidder()).toEqual(expect.any(Object));
      expect(harness.registerPucGamAttempt).not.toHaveBeenCalled();
    } finally {
      harness.dispose();
    }
  });

  it('rejects a takeover that omits the selected initial Prebid state', async () => {
    const harness = initialProductionPrebidHarness([]);
    try {
      const prepared = await createPrebidIntegrationRegistration(RELEASE_ID).prepare(
        harness.prepareContext
      );
      const adoption = Object.freeze({
        version: 1 as const,
        adoptInitialDisplay: true as const,
        handoff: Object.freeze({
          slices: Object.freeze(['first_display', 'prebid_initial']),
          parserState: Object.freeze([]),
        }),
        identities: Object.freeze([]),
      });

      expect(() =>
        prepared.activate(Object.freeze({ ...harness.activationContext, adoption }))
      ).toThrow('Prebid first-display adoption is invalid');
    } finally {
      harness.dispose();
    }
  });

  it('publishes the initial TS winner and promotes its exact selection through render.v1', async () => {
    const harness = initialProductionPrebidHarness([
      Object.freeze({
        moduleName: 'sharedIdSystem',
        configNames: Object.freeze(['sharedId']),
        eidSources: Object.freeze(['sharedid.org']),
      }),
    ]);
    try {
      const prepared = await createPrebidIntegrationRegistration(RELEASE_ID).prepare(
        harness.prepareContext
      );
      prepared.activate(harness.activationContext);
      for (const callback of harness.afterCommit) callback();
      const bidder = harness.binding.bidder();
      if (!bidder) throw new Error('Expected trustedServer bidder registration');
      const done = vi.fn();
      let admitted: Readonly<Record<string, unknown>> | undefined;
      bidder.callBids(
        Object.freeze({
          auctionId: 'auction-one',
          bids: Object.freeze([
            Object.freeze({
              adUnitCode: 'slot-one',
              adUnitId: 'unit-one',
              auctionId: 'auction-one',
              bidId: 'request-one',
              src: 'client',
              transactionId: 'transaction-one',
            }),
          ]),
        }),
        (adUnitCode, response) => {
          const enriched = Object.freeze({ ...response, adUnitCode });
          admitted = enriched;
          harness.binding.addResponse(adUnitCode, enriched);
        },
        done
      );
      expect(done).toHaveBeenCalledOnce();
      expect(admitted).toMatchObject({
        adId: harness.bid.rendererReservationId,
        bidderCode: 'trustedServer',
        requestId: 'request-one',
      });
      const selected = admitted;
      if (!selected) throw new Error('Expected admitted TS bid');
      harness.binding.select([
        Object.freeze({
          ...selected,
          adUnitCode: 'slot-one',
          auctionId: 'auction-one',
        }),
      ]);
      expect(harness.reservations.recognize(harness.bid.rendererReservationId)).toMatchObject({
        state: 'awaiting_prebid_selection',
      });
      harness.binding.emit('auctionEnd', Object.freeze({ auctionId: 'auction-one' }));
      expect(harness.binding.pbjs.getHighestCpmBids).toHaveBeenCalledOnce();
      expect(harness.registerPucGamAttempt).toHaveBeenCalledOnce();
      expect(harness.reservations.recognize(harness.bid.rendererReservationId)).toMatchObject({
        state: 'renderable',
      });
    } finally {
      harness.dispose();
    }
  });
});

describe('transactional test-composition Prebid boundary', () => {
  it('prepares inertly, activates reversible listeners, and starts only after commit', async () => {
    const config = Object.freeze({ clientSideBidders: Object.freeze(['rubicon']) });
    const order: string[] = [];
    const start = vi.fn((received: unknown) => {
      order.push('start');
      expect(received).toBe(config);
    });
    const release = vi.fn(() => order.push('release'));
    const activate = vi.fn(() => {
      order.push('prebid:activate');
      return release;
    });
    let finishPreparation: (() => void) | undefined;
    const preparationGate = new Promise<void>((resolve) => {
      finishPreparation = resolve;
    });
    const registry = createIntegrationRegistry({
      manifest: manifest(['prebid', 'gate']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['prebid', 'gate']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({ prebid: Object.freeze({ activate, start }) }),
      }),
    });
    registry.register(createLegacyPrebidRegistrationForTest(RELEASE_ID));
    registry.register(
      registration('gate', async () => {
        order.push('gate:prepare');
        await preparationGate;
        return Object.freeze({ activate: () => order.push('gate:activate') });
      })
    );

    const installing = registry.install(callbacks(order));
    await vi.waitFor(() => expect(order).toEqual(['gate:prepare']));
    expect(start).not.toHaveBeenCalled();

    finishPreparation?.();
    const result = await installing;

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'gate:prepare',
      'core',
      'prebid:activate',
      'gate:activate',
      'publish',
      'start',
      'drain',
    ]);
    expect(activate).toHaveBeenCalledTimes(1);
    expect(start).toHaveBeenCalledExactlyOnceWith(config);
    if (result.state === 'kernel') {
      result.dispose();
      result.dispose();
    }
    expect(release).toHaveBeenCalledTimes(1);
  });

  it('unwinds Prebid activation before fallback when a later module fails', async () => {
    const release = vi.fn();
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['prebid', 'broken']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['prebid', 'broken']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({}),
        interfaces: Object.freeze({
          prebid: Object.freeze({ activate: () => release, start }),
        }),
      }),
    });
    registry.register(createLegacyPrebidRegistrationForTest(RELEASE_ID));
    registry.register(
      registration('broken', () => ({
        activate: () => {
          throw new Error('fictional activation failure');
        },
      }))
    );

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(release).toHaveBeenCalledTimes(1);
    expect(start).not.toHaveBeenCalled();
  });

  it('does not start when reversible Prebid activation fails', async () => {
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['prebid']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['prebid']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({}),
        interfaces: Object.freeze({
          prebid: Object.freeze({
            activate: () => {
              throw new Error('fictional listener activation failure');
            },
            start,
          }),
        }),
      }),
    });
    registry.register(createLegacyPrebidRegistrationForTest(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(start).not.toHaveBeenCalled();
  });

  it('fails preparation without effects when the composition omits the Prebid boundary', async () => {
    const registry = createIntegrationRegistry({
      manifest: manifest(['prebid']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['prebid']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
    });
    registry.register(createLegacyPrebidRegistrationForTest(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
  });

  it.each([
    [
      'accessor',
      Object.freeze(
        Object.defineProperty({}, 'externalBundleUrl', {
          enumerable: true,
          get: () => '/publisher-controlled',
        })
      ),
    ],
    ['mutable nested data', Object.freeze({ nested: {} })],
    ['non-plain data', Object.freeze({ value: Object.freeze(new Date(0)) })],
  ])('rejects %s configuration during inert preparation', async (_caseName, config) => {
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['prebid']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['prebid']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({
          prebid: Object.freeze({ activate: () => vi.fn(), start }),
        }),
      }),
    });
    registry.register(createLegacyPrebidRegistrationForTest(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(start).not.toHaveBeenCalled();
  });

  it('isolates post-commit startup failure to the Prebid module', async () => {
    const start = vi.fn(() => {
      throw new Error('fictional Prebid startup failure');
    });
    const runtimeFailures: unknown[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(['prebid']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['prebid']),
      startedAtMs: 0,
      now: () => 0,
      onRuntimeFailure: (failure) => runtimeFailures.push(failure),
      getBindings: () => ({
        config: Object.freeze({}),
        interfaces: Object.freeze({
          prebid: Object.freeze({ activate: () => vi.fn(), start }),
        }),
      }),
    });
    registry.register(createLegacyPrebidRegistrationForTest(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'kernel',
      runtimeFailures: [{ id: 'prebid', phase: 'after_commit' }],
    });
    expect(start).toHaveBeenCalledTimes(1);
    expect(runtimeFailures).toEqual([{ id: 'prebid', phase: 'after_commit' }]);
  });
});

describe('RCJ-PREBID-04 refresh policy', () => {
  function refreshHarness(
    excludedGamAdUnitPathSuffixes: readonly string[] | (() => readonly string[])
  ) {
    const runtime = createRuntimeSession({
      createIdentityIssuer: () =>
        createTestNavigationIdentityIssuer({
          getRandomValues: (target) => {
            target.fill(3);
            return target;
          },
        }),
    });
    const navigationResult = runtime.startInitialNavigation();
    if (!navigationResult.ok) throw new Error('Expected navigation');
    const navigation = navigationResult.value;
    const clearCalls: Array<readonly [object, string]> = [];
    const operationDisposals: Array<ReturnType<typeof vi.fn>> = [];
    const googletag = {
      run: vi.fn((command: (gpt: object) => unknown) => {
        const dispose = vi.fn();
        operationDisposals.push(dispose);
        const facade = Object.freeze({
          adUnitPath: (slot: object) => {
            const getter = Reflect.get(slot, 'getAdUnitPath');
            if (typeof getter !== 'function') return undefined;
            return Reflect.apply(getter, slot, []);
          },
          clearTargeting: (slot: object, key: string) => {
            clearCalls.push([slot, key]);
            const clear = Reflect.get(slot, 'clearTargeting');
            if (typeof clear === 'function') return Reflect.apply(clear, slot, [key]);
            return undefined;
          },
        });
        return Object.freeze({
          status: 'present' as const,
          result: Promise.resolve(command(facade)),
          dispose,
        });
      }),
    };
    const auctionDisposals: Array<ReturnType<typeof vi.fn>> = [];
    const runSyntheticAuction = vi.fn((_slots: readonly object[]) => {
      const dispose = vi.fn();
      auctionDisposals.push(dispose);
      return Object.freeze({ completion: Promise.resolve(), dispose });
    });
    const policy = createPrebidRefreshPolicy({
      currentNavigation: () => navigation,
      excludedGamAdUnitPathSuffixes,
      googletag: googletag as unknown as Pick<GoogletagAdapter, 'run'>,
      runSyntheticAuction,
    });
    return {
      auctionDisposals,
      clearCalls,
      navigation,
      operationDisposals,
      policy,
      runSyntheticAuction,
      runtime,
    };
  }

  it('clears every target then filters only literal case-sensitive suffix matches', async () => {
    const harness = refreshHarness(['/tracking']);
    const excluded = {
      clearTargeting: vi.fn(),
      getAdUnitPath: vi.fn(() => '/network/tracking'),
    };
    const caseMismatch = {
      clearTargeting: vi.fn(),
      getAdUnitPath: vi.fn(() => '/network/Tracking'),
    };
    const trailingSlash = {
      clearTargeting: vi.fn(),
      getAdUnitPath: vi.fn(() => '/network/tracking/'),
    };
    const missing = { clearTargeting: vi.fn() };
    const nonString = {
      clearTargeting: vi.fn(),
      getAdUnitPath: vi.fn(() => 42),
    };
    const throwing = {
      clearTargeting: vi.fn(),
      getAdUnitPath: vi.fn(() => {
        throw new Error('path unavailable');
      }),
    };
    const clearFailure = {
      clearTargeting: vi.fn((key: string) => {
        if (key === 'hb_adid') throw new Error('clear unavailable');
      }),
      getAdUnitPath: vi.fn(() => '/network/tracking'),
    };
    const slots = Object.freeze([
      excluded,
      caseMismatch,
      trailingSlash,
      missing,
      nonString,
      throwing,
      clearFailure,
    ]);

    await harness.policy.prepare(
      Object.freeze({ requestedSlots: slots, slots, options: Object.freeze({ exact: true }) })
    );

    const expectedKeys = [
      'ts_initial',
      'hb_pb',
      'hb_bidder',
      'hb_adid',
      'hb_cache_host',
      'hb_cache_path',
    ];
    for (const slot of slots) {
      expect(
        harness.clearCalls.filter(([target]) => target === slot).map(([, key]) => key)
      ).toEqual(expectedKeys);
    }
    expect(harness.runSyntheticAuction).toHaveBeenCalledExactlyOnceWith(
      [caseMismatch, trailingSlash, missing, nonString, throwing, clearFailure],
      harness.navigation
    );
    harness.policy.dispose();
    harness.runtime.dispose();
  });

  it('skips the synthetic auction when all targets are excluded', async () => {
    const harness = refreshHarness(['/skip']);
    const slots = Object.freeze([
      { getAdUnitPath: () => '/one/skip' },
      { getAdUnitPath: () => '/two/skip' },
    ]);

    await harness.policy.prepare(
      Object.freeze({ requestedSlots: undefined, slots, options: undefined })
    );

    expect(harness.runSyntheticAuction).not.toHaveBeenCalled();
    expect(harness.clearCalls).toHaveLength(slots.length * 6);
    harness.policy.dispose();
    harness.runtime.dispose();
  });

  it('reads the configured exclusion snapshot only when the activated policy prepares', async () => {
    let configuredSuffixes: readonly string[] = Object.freeze([]);
    const harness = refreshHarness(() => configuredSuffixes);
    configuredSuffixes = Object.freeze(['/configured-after-activation']);
    const slot = Object.freeze({ getAdUnitPath: () => '/network/configured-after-activation' });

    await harness.policy.prepare(
      Object.freeze({ requestedSlots: Object.freeze([slot]), slots: Object.freeze([slot]) })
    );

    expect(harness.runSyntheticAuction).not.toHaveBeenCalled();
    expect(harness.clearCalls).toHaveLength(6);
    harness.policy.dispose();
    harness.runtime.dispose();
  });

  it('settles pending work on navigation abort and ignores a late auction completion', async () => {
    const harness = refreshHarness([]);
    let finishAuction!: () => void;
    const auction = new Promise<void>((resolve) => {
      finishAuction = resolve;
    });
    const auctionDispose = vi.fn();
    harness.runSyntheticAuction.mockReturnValue(
      Object.freeze({ completion: auction, dispose: auctionDispose })
    );
    const slot = Object.freeze({ getAdUnitPath: () => '/eligible' });
    const completion = harness.policy.prepare(
      Object.freeze({
        requestedSlots: Object.freeze([slot]),
        slots: Object.freeze([slot]),
        options: undefined,
      })
    );
    await vi.waitFor(() => expect(harness.runSyntheticAuction).toHaveBeenCalledOnce());

    harness.runtime.replaceNavigation();
    await expect(completion).resolves.toBeUndefined();
    expect(harness.operationDisposals[0]).toHaveBeenCalledOnce();
    expect(auctionDispose).toHaveBeenCalledOnce();
    finishAuction();
    await auction;
    await Promise.resolve();
    expect(harness.runSyntheticAuction).toHaveBeenCalledOnce();
    harness.policy.dispose();
  });

  it('settles pending work when the refresh policy is disposed', async () => {
    const harness = refreshHarness([]);
    let finishAuction!: () => void;
    const auction = new Promise<void>((resolve) => {
      finishAuction = resolve;
    });
    const auctionDispose = vi.fn();
    harness.runSyntheticAuction.mockReturnValue(
      Object.freeze({ completion: auction, dispose: auctionDispose })
    );
    const slot = Object.freeze({ getAdUnitPath: () => '/eligible' });
    const completion = harness.policy.prepare(
      Object.freeze({ requestedSlots: Object.freeze([slot]), slots: Object.freeze([slot]) })
    );
    await vi.waitFor(() => expect(harness.runSyntheticAuction).toHaveBeenCalledOnce());

    harness.policy.dispose();
    harness.policy.dispose();
    await expect(completion).resolves.toBeUndefined();
    expect(harness.operationDisposals[0]).toHaveBeenCalledOnce();
    expect(auctionDispose).toHaveBeenCalledOnce();
    finishAuction();
    await auction;
    harness.runtime.dispose();
  });
});

describe('RCJ-PREBID-04 adapter-backed synthetic refresh runner', () => {
  it('routes detached server and client bids without consulting publisher Prebid state', () => {
    const slot = Object.freeze({ id: 'slot-a' });
    const serverParams = Object.freeze({ placement: 'current' });
    const unit = Object.freeze({
      code: 'slot-a',
      mediaTypes: Object.freeze({
        banner: Object.freeze({ sizes: Object.freeze([Object.freeze([300, 250])]) }),
      }),
      bids: Object.freeze([
        Object.freeze({
          bidder: 'trustedServer',
          params: Object.freeze({
            bidderParams: Object.freeze({
              client: Object.freeze({ stale: true }),
              preserved: Object.freeze({ placement: 'folded' }),
              server: Object.freeze({ placement: 'stale' }),
            }),
            zone: 'news',
          }),
        }),
        Object.freeze({ bidder: 'server', params: serverParams }),
        Object.freeze({ bidder: 'client', params: Object.freeze({ placement: 'browser' }) }),
      ]),
    });

    const prepared = preparePrebidRegisteredRefreshAuction({
      clientSideBidders: Object.freeze(['client']),
      resolveAdUnit: (candidate) => (candidate === slot ? unit : undefined),
      slots: Object.freeze([slot]),
    });

    expect(prepared).toEqual({
      adUnitCodes: ['slot-a'],
      adUnits: [
        {
          code: 'slot-a',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          bids: [
            {
              bidder: 'trustedServer',
              params: {
                bidderParams: {
                  preserved: { placement: 'folded' },
                  server: { placement: 'current' },
                },
                zone: 'news',
              },
            },
            { bidder: 'client', params: { placement: 'browser' } },
          ],
        },
      ],
    });
    expect(Object.isFrozen(prepared?.adUnits)).toBe(true);
    expect(Object.isFrozen(prepared?.adUnits[0])).toBe(true);
  });

  it('preserves legacy last-write precedence when folded params follow direct bids', () => {
    const slot = Object.freeze({ id: 'slot-order' });
    const unit = Object.freeze({
      code: 'slot-order',
      mediaTypes: Object.freeze({ banner: Object.freeze({ sizes: Object.freeze([]) }) }),
      bids: Object.freeze([
        Object.freeze({
          bidder: 'server',
          params: Object.freeze({ placement: 'direct-first' }),
        }),
        Object.freeze({
          bidder: 'trustedServer',
          params: Object.freeze({
            bidderParams: Object.freeze({
              preserved: Object.freeze({ placement: 'folded-only' }),
              server: Object.freeze({ placement: 'folded-last' }),
            }),
          }),
        }),
      ]),
    });

    const prepared = preparePrebidRegisteredRefreshAuction({
      clientSideBidders: Object.freeze([]),
      resolveAdUnit: () => unit,
      slots: Object.freeze([slot]),
    });

    expect(prepared?.adUnits).toEqual([
      {
        code: 'slot-order',
        mediaTypes: { banner: { sizes: [] } },
        bids: [
          {
            bidder: 'trustedServer',
            params: {
              bidderParams: {
                server: { placement: 'folded-last' },
                preserved: { placement: 'folded-only' },
              },
            },
          },
        ],
      },
    ]);
    const bidderParams = (
      prepared?.adUnits[0] as {
        bids: readonly [{ params: { bidderParams: Readonly<Record<string, unknown>> } }];
      }
    ).bids[0].params.bidderParams;
    expect(Object.keys(bidderParams)).toEqual(['server', 'preserved']);
  });

  it('fails closed when detached registrations contain duplicate trustedServer bids', () => {
    const slot = Object.freeze({ id: 'slot-duplicate-trusted' });
    const trustedBid = Object.freeze({
      bidder: 'trustedServer',
      params: Object.freeze({ bidderParams: Object.freeze({}) }),
    });
    const unit = Object.freeze({
      code: 'slot-duplicate-trusted',
      mediaTypes: Object.freeze({ banner: Object.freeze({ sizes: Object.freeze([]) }) }),
      bids: Object.freeze([trustedBid, trustedBid]),
    });

    expect(
      preparePrebidRegisteredRefreshAuction({
        clientSideBidders: Object.freeze([]),
        resolveAdUnit: () => unit,
        slots: Object.freeze([slot]),
      })
    ).toBeUndefined();
  });

  it('keeps deterministic order while resolving duplicate direct and client bids', () => {
    const slot = Object.freeze({ id: 'slot-duplicates' });
    const unit = Object.freeze({
      code: 'slot-duplicates',
      mediaTypes: Object.freeze({ banner: Object.freeze({ sizes: Object.freeze([]) }) }),
      bids: Object.freeze([
        Object.freeze({ bidder: 'alpha', params: Object.freeze({ sequence: 1 }) }),
        Object.freeze({ bidder: 'client', params: Object.freeze({ sequence: 1 }) }),
        Object.freeze({ bidder: 'beta', params: Object.freeze({ sequence: 1 }) }),
        Object.freeze({ bidder: 'alpha', params: Object.freeze({ sequence: 2 }) }),
        Object.freeze({ bidder: 'client', params: Object.freeze({ sequence: 2 }) }),
      ]),
    });

    const prepared = preparePrebidRegisteredRefreshAuction({
      clientSideBidders: Object.freeze(['client']),
      resolveAdUnit: () => unit,
      slots: Object.freeze([slot]),
    });

    expect(prepared?.adUnits).toEqual([
      {
        code: 'slot-duplicates',
        mediaTypes: { banner: { sizes: [] } },
        bids: [
          {
            bidder: 'trustedServer',
            params: { bidderParams: { alpha: { sequence: 2 }, beta: { sequence: 1 } } },
          },
          { bidder: 'client', params: { sequence: 1 } },
          { bidder: 'client', params: { sequence: 2 } },
        ],
      },
    ]);
    const bidderParams = (
      prepared?.adUnits[0] as {
        bids: readonly [{ params: { bidderParams: Readonly<Record<string, unknown>> } }];
      }
    ).bids[0].params.bidderParams;
    expect(Object.keys(bidderParams)).toEqual(['alpha', 'beta']);
  });

  it('returns a recursively frozen synthetic refresh preparation', () => {
    const slot = Object.freeze({ id: 'slot-frozen' });
    const unit = Object.freeze({
      code: 'slot-frozen',
      mediaTypes: Object.freeze({
        banner: Object.freeze({ sizes: Object.freeze([Object.freeze([300, 250])]) }),
      }),
      bids: Object.freeze([
        Object.freeze({
          bidder: 'server',
          params: Object.freeze({
            placement: Object.freeze({
              rules: Object.freeze([Object.freeze({ label: 'frozen' })]),
            }),
          }),
        }),
      ]),
    });
    const prepared = preparePrebidRegisteredRefreshAuction({
      clientSideBidders: Object.freeze([]),
      resolveAdUnit: () => unit,
      slots: Object.freeze([slot]),
    });
    const seen = new Set<object>();
    const expectRecursivelyFrozen = (value: unknown): void => {
      if (value === null || typeof value !== 'object' || seen.has(value)) return;
      seen.add(value);
      expect(Object.isFrozen(value)).toBe(true);
      for (const child of Object.values(value)) expectRecursivelyFrozen(child);
    };

    expect(prepared).toBeDefined();
    expectRecursivelyFrozen(prepared);
  });

  it('fails closed when a physical slot has no detached registered ad unit', () => {
    const slot = Object.freeze({ id: 'unregistered' });

    expect(
      preparePrebidRegisteredRefreshAuction({
        clientSideBidders: Object.freeze([]),
        resolveAdUnit: () => undefined,
        slots: Object.freeze([slot]),
      })
    ).toBeUndefined();
  });

  function runnerHarness(options: Readonly<{ requestThrows?: boolean }> = {}) {
    const runtime = createRuntimeSession({
      createIdentityIssuer: () =>
        createTestNavigationIdentityIssuer({
          getRandomValues: (target) => {
            target.fill(4);
            return target;
          },
        }),
    });
    const navigationResult = runtime.startInitialNavigation();
    if (!navigationResult.ok) throw new Error('Expected navigation');
    const navigation = navigationResult.value;
    const order: string[] = [];
    let requestOptions:
      | Readonly<{
          adUnits: readonly object[];
          bidsBackHandler: () => void;
          timeout: number;
        }>
      | undefined;
    const facade = Object.freeze({
      requestBids: vi.fn((received: unknown) => {
        order.push('request');
        if (options.requestThrows) throw new Error('request unavailable');
        requestOptions = received as typeof requestOptions;
      }),
      setTargetingForGpt: vi.fn((codes: readonly string[]) => {
        order.push(`target:${codes.join(',')}`);
      }),
    }) as unknown as Readonly<PrebidFacade>;
    const adapterDispose = vi.fn();
    const prebid = Object.freeze({
      run: vi.fn((command: (prebid: Readonly<PrebidFacade>) => unknown) =>
        Object.freeze({
          status: 'present' as const,
          result: Promise.resolve(command(facade)),
          dispose: adapterDispose,
        })
      ),
    }) as unknown as Pick<PrebidAdapter, 'run'>;
    let deadline: (() => void) | undefined;
    const timerHandle = Object.freeze({});
    const clear = vi.fn();
    const slot = Object.freeze({ id: 'slot-a' });
    const adUnit = Object.freeze({ code: 'slot-a', bids: Object.freeze([]) });
    const prepareAuction = vi.fn(() =>
      Object.freeze({
        adUnitCodes: Object.freeze(['slot-a']),
        adUnits: Object.freeze([adUnit]),
      })
    );
    const runner = createPrebidSyntheticRefreshRunner({
      prebid,
      prepareAuction,
      scheduler: Object.freeze({
        clear,
        set: (callback: () => void, milliseconds: number) => {
          expect(milliseconds).toBe(1_500);
          deadline = callback;
          return timerHandle;
        },
      }),
    });
    return {
      adapterDispose,
      clear,
      deadline: () => deadline,
      facade,
      navigation,
      order,
      prepareAuction,
      requestOptions: () => requestOptions,
      runner,
      runtime,
      slot,
      timerHandle,
    };
  }

  it('requests eligible ad units then applies only their scoped targeting before completion', async () => {
    const harness = runnerHarness();
    const operation = harness.runner(Object.freeze([harness.slot]), harness.navigation);

    expect(harness.order).toEqual(['request']);
    expect(harness.prepareAuction).toHaveBeenCalledExactlyOnceWith(
      [harness.slot],
      harness.navigation
    );
    expect(harness.requestOptions()).toMatchObject({
      adUnits: [{ code: 'slot-a', bids: [] }],
      timeout: 1_500,
    });
    harness.requestOptions()?.bidsBackHandler();
    await expect(operation.completion).resolves.toBeUndefined();

    expect(harness.order).toEqual(['request', 'target:slot-a']);
    expect(harness.clear).toHaveBeenCalledExactlyOnceWith(harness.timerHandle);
    expect(harness.adapterDispose).toHaveBeenCalledOnce();
    harness.runtime.dispose();
  });

  it('uses one targeting/settlement latch for timeout, disposal, and late callbacks', async () => {
    const timedOut = runnerHarness();
    const timedOutOperation = timedOut.runner(Object.freeze([timedOut.slot]), timedOut.navigation);
    const lateTimeoutCallback = timedOut.requestOptions()?.bidsBackHandler;
    timedOut.deadline()?.();
    await expect(timedOutOperation.completion).resolves.toBeUndefined();
    lateTimeoutCallback?.();
    expect(timedOut.order).toEqual(['request', 'target:slot-a']);
    expect(timedOut.adapterDispose).toHaveBeenCalledOnce();
    timedOut.runtime.dispose();

    const disposed = runnerHarness();
    const disposedOperation = disposed.runner(Object.freeze([disposed.slot]), disposed.navigation);
    const lateDisposedCallback = disposed.requestOptions()?.bidsBackHandler;
    disposedOperation.dispose();
    disposedOperation.dispose();
    await expect(disposedOperation.completion).resolves.toBeUndefined();
    lateDisposedCallback?.();
    disposed.deadline()?.();
    expect(disposed.order).toEqual(['request']);
    expect(disposed.adapterDispose).toHaveBeenCalledOnce();
    disposed.runtime.dispose();
  });

  it('forwards completion without targeting when requestBids throws', async () => {
    const harness = runnerHarness({ requestThrows: true });
    const operation = harness.runner(Object.freeze([harness.slot]), harness.navigation);

    await expect(operation.completion).resolves.toBeUndefined();
    expect(harness.order).toEqual(['request']);
    expect(harness.facade.setTargetingForGpt).not.toHaveBeenCalled();
    expect(harness.adapterDispose).toHaveBeenCalledOnce();
    expect(harness.deadline()).toBeUndefined();
    harness.runtime.dispose();
  });
});

describe('ordered Prebid bid publication', () => {
  function preparePublication() {
    const runtime = createRuntimeSession({
      createIdentityIssuer: () =>
        createTestNavigationIdentityIssuer({
          getRandomValues: (target) => {
            target.fill(1);
            return target;
          },
        }),
    });
    const navigationResult = runtime.startInitialNavigation();
    if (!navigationResult.ok) throw new Error('Expected navigation');
    const navigation = navigationResult.value;
    const reservationId = `r1_${'a'.repeat(22)}`;
    const renderSource = Object.freeze({
      type: 'adm' as const,
      version: 1 as const,
      adm: '<main>private creative</main>',
      width: 300,
      height: 250,
    });
    const bid = Object.freeze({
      candidateId: 'AAAAAAAAAAAA',
      slot: 'slot-one',
      provider: 'aps',
      upstreamBidId: 'upstream-one',
      cpm: 1.25,
      currency: 'USD' as const,
      targeting: Object.freeze({ hb_bidder: 'trustedServer' }),
      rendererReservationId: reservationId,
      renderSource,
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
    expect(navigation.installAuctionProjection(projection)).toBe(true);
    const reservations = createReservationService({
      prepareRenderSource: (candidate) =>
        typeof candidate === 'object' && candidate !== null && Object.isFrozen(candidate)
          ? (candidate as typeof renderSource)
          : undefined,
    });
    const generatedBid = Object.freeze({
      requestId: 'prebid-request-one',
      adId: 'prebid-generated-id',
      cpm: bid.cpm,
      width: 300,
      height: 250,
    });
    const order: string[] = [];
    const admitTrustedBid = vi.fn((_preparedBid: Readonly<PreparedTrustedBidV1>) => {
      order.push('admit');
      expect(reservations.recognize(reservationId)).toMatchObject({
        recognized: true,
        state: 'awaiting_prebid_selection',
      });
      return 'admitted' as const;
    });
    const trackAdmittedBid = vi.fn(() => {
      order.push('track');
      return true;
    });
    const input: PrebidBidPublicationInput = {
      admitTrustedBid,
      auctionId: 'auction-one',
      adUnitCode: bid.slot,
      bid,
      generatedBid,
      navigation,
      reservations: {
        registerPrebidLease: (registrationInput) => {
          order.push('reservation');
          return reservations.registerPrebidLease(registrationInput);
        },
        tombstonePrebidLease: reservations.tombstonePrebidLease,
      },
      trackAdmittedBid,
    };
    return {
      admitTrustedBid,
      bid,
      generatedBid,
      input,
      navigation,
      order,
      reservationId,
      reservations,
      runtime,
      trackAdmittedBid,
    };
  }

  it('registers the lease before exposing one capability-free frozen bid', () => {
    const publication = preparePublication();

    const result = publishPrebidBid(publication.input);

    expect(result.ok).toBe(true);
    expect(publication.order).toEqual(['reservation', 'admit', 'track']);
    expect(publication.admitTrustedBid).toHaveBeenCalledTimes(1);
    const prepared = publication.admitTrustedBid.mock.calls[0]?.[0];
    if (!prepared) throw new Error('Expected prepared bid');
    expect(prepared).toMatchObject({
      auctionId: 'auction-one',
      adUnitCode: 'slot-one',
      bid: {
        requestId: 'prebid-request-one',
        adId: publication.reservationId,
        cpm: 1.25,
        width: 300,
        height: 250,
        ad: '',
        ttl: 300,
        creativeId: 'upstream-one',
        netRevenue: true,
        currency: 'USD',
        bidderCode: 'trustedServer',
        meta: {
          advertiserDomains: [],
          tsAuctionId: 'auction-one',
          tsBidId: 'upstream-one',
        },
      },
    });
    expect(Object.isFrozen(prepared)).toBe(true);
    expect(Object.isFrozen(prepared.bid)).toBe(true);
    expect(Object.isFrozen(prepared.bid.meta)).toBe(true);
    expect(JSON.stringify(prepared)).not.toContain('private creative');
    expect(publication.generatedBid.adId).toBe('prebid-generated-id');
    publication.runtime.dispose();
  });

  it('suppresses a partially published bid or failed selection tracking as a contract violation', () => {
    const partial = preparePublication();
    expect(
      publishPrebidBid({
        ...partial.input,
        admitTrustedBid: () => {
          throw new PrebidAdmissionContractError();
        },
      })
    ).toEqual({ ok: false, reason: 'prebid_contract_violation' });
    expect(partial.reservations.recognize(partial.reservationId)).toMatchObject({
      state: 'prebid_contract_violation',
    });
    partial.runtime.dispose();

    const untracked = preparePublication();
    expect(publishPrebidBid({ ...untracked.input, trackAdmittedBid: () => false })).toEqual({
      ok: false,
      reason: 'prebid_contract_violation',
    });
    expect(untracked.reservations.recognize(untracked.reservationId)).toMatchObject({
      state: 'prebid_contract_violation',
    });
    untracked.runtime.dispose();
  });

  it.each([
    ['not admitted', () => 'not_admitted' as const, 'prebid_admission_failed'],
    [
      'throw',
      () => {
        throw new Error('fictional Prebid failure');
      },
      'prebid_admission_failed',
    ],
    ['partial publication', () => 'partially_admitted', 'prebid_contract_violation'],
  ])('tombstones an admission that reports %s', (_caseName, admission, reason) => {
    const publication = preparePublication();

    expect(publishPrebidBid({ ...publication.input, admitTrustedBid: admission })).toEqual({
      ok: false,
      reason,
    });
    expect(publication.reservations.recognize(publication.reservationId)).toMatchObject({
      recognized: true,
      state: reason,
    });
    publication.runtime.dispose();
  });

  it('fails before exposure on collision and leaves the generated identity untouched', () => {
    const publication = preparePublication();
    expect(
      publication.reservations.registerPrebidLease({
        reservationId: publication.reservationId,
        slot: publication.bid.slot,
        navigation: publication.navigation,
        auctionId: 'auction-one',
        adUnitCode: publication.bid.slot,
        renderSource: publication.bid.renderSource,
        winnerContext: Object.freeze({ selectedCpm: publication.bid.cpm }),
        prebidBid: Object.freeze({ cpm: publication.bid.cpm }),
      })
    ).toMatchObject({ ok: true });

    expect(publishPrebidBid(publication.input)).toEqual({
      ok: false,
      reason: 'reservation_collision',
    });
    expect(publication.admitTrustedBid).not.toHaveBeenCalled();
    expect(publication.generatedBid.adId).toBe('prebid-generated-id');
    publication.runtime.dispose();
  });

  it('rejects a stale projected bid and malformed generated response before registration', () => {
    const stale = preparePublication();
    expect(publishPrebidBid({ ...stale.input, auctionId: 'other-auction' })).toEqual({
      ok: false,
      reason: 'winner_not_renderable',
    });
    expect(stale.order).toEqual([]);
    stale.runtime.dispose();

    const malformed = preparePublication();
    expect(publishPrebidBid({ ...malformed.input, generatedBid: { cpm: 1.25 } })).toEqual({
      ok: false,
      reason: 'descriptor_invalid',
    });
    expect(malformed.order).toEqual([]);
    malformed.runtime.dispose();
  });
});

describe('Prebid selection coordination', () => {
  function prepareSelection(
    options: Readonly<{
      activateResult?: boolean;
      synchronousTimer?: boolean;
      throwCreateAttempt?: boolean;
      throwFail?: boolean;
      throwPromotion?: boolean;
    }> = {}
  ) {
    let now = 0;
    const runtime = createRuntimeSession({
      createIdentityIssuer: () =>
        createTestNavigationIdentityIssuer({
          getRandomValues: (target) => {
            target.fill(7);
            return target;
          },
        }),
    });
    const navigationResult = runtime.startInitialNavigation();
    if (!navigationResult.ok) throw new Error('Expected navigation');
    const navigation = navigationResult.value;
    const reservations = createReservationService({
      now: () => now,
      prepareRenderSource: (candidate) =>
        typeof candidate === 'object' && candidate !== null && Object.isFrozen(candidate)
          ? (candidate as Readonly<{ type: 'aps' | 'adm'; version: 1 }>)
          : undefined,
    });
    const artifacts = createCommittedArtifactStore();
    const attempts: RenderAttempt[] = [];
    const promotions: Array<ReturnType<typeof reservations.promotePrebidSelection>> = [];
    const attemptOwners: RenderAttemptScope[] = [];
    const timers = new Map<object, () => void>();
    const cleared: object[] = [];
    const activateAttempt = vi.fn(() => options.activateResult ?? true);
    const coordinator = createPrebidSelectionCoordinator({
      activateAttempt,
      createAttempt: (owner) => {
        if (options.throwCreateAttempt) throw new Error('attempt factory failed');
        attemptOwners.push(owner);
        const result = createRenderAttempt({
          artifacts,
          owner,
          prepareRenderSource: (candidate) =>
            typeof candidate === 'object' && candidate !== null && Object.isFrozen(candidate)
              ? (candidate as Readonly<{ type: 'aps' | 'adm'; version: 1 }>)
              : undefined,
          reservations,
        });
        if (result.ok) {
          attempts.push(result.value);
          if (options.throwFail) {
            return Object.freeze({
              ok: true as const,
              value: Object.freeze({
                ...result.value,
                fail: () => {
                  throw new Error('attempt failure settlement failed');
                },
              }),
            });
          }
        }
        return result;
      },
      reservations: {
        promotePrebidSelection: (input) => {
          if (options.throwPromotion) throw new Error('promotion failed');
          const result = reservations.promotePrebidSelection(input);
          promotions.push(result);
          return result;
        },
        tombstone: reservations.tombstone,
        tombstonePrebidGroup: reservations.tombstonePrebidGroup,
      },
      scheduler: {
        clear: (handle) => {
          cleared.push(handle as object);
          timers.delete(handle as object);
        },
        set: (callback, milliseconds) => {
          expect(milliseconds).toBe(10_000);
          const handle = Object.freeze({});
          timers.set(handle, callback);
          if (options.synchronousTimer) callback();
          return handle;
        },
      },
    });
    const admitted = (idCharacter: string, adUnitCode = 'slot-one') => {
      const reservationId = `r1_${idCharacter.repeat(22)}`;
      const bid = Object.freeze({
        requestId: `request-${idCharacter}`,
        adId: reservationId,
        cpm: 1.25,
        width: 300,
        height: 250,
        ad: '' as const,
        ttl: 300 as const,
        creativeId: `creative-${idCharacter}`,
        netRevenue: true as const,
        currency: 'USD' as const,
        bidderCode: 'trustedServer' as const,
        meta: Object.freeze({
          advertiserDomains: Object.freeze([] as string[]),
          tsAuctionId: 'auction-one',
          tsBidId: `bid-${idCharacter}`,
        }),
      });
      const prepared = Object.freeze({ auctionId: 'auction-one', adUnitCode, bid });
      const renderSource = Object.freeze({
        type: 'adm' as const,
        version: 1 as const,
        adm: `<main>${idCharacter}</main>`,
        width: 300,
        height: 250,
      });
      expect(
        reservations.registerPrebidLease({
          reservationId,
          slot: adUnitCode,
          navigation,
          auctionId: prepared.auctionId,
          adUnitCode,
          renderSource,
          winnerContext: Object.freeze({ selectedCpm: bid.cpm }),
          prebidBid: bid,
        })
      ).toMatchObject({ ok: true });
      expect(coordinator.track(prepared, navigation)).toBe(!options.synchronousTimer);
      return prepared;
    };
    return {
      admitted,
      activateAttempt,
      attempts,
      attemptOwners,
      cleared,
      coordinator,
      navigation,
      promotions,
      reservations,
      runtime,
      setNow: (value: number) => {
        now = value;
      },
      timers,
    };
  }

  it('contains a hostile publication failure settlement and releases its ephemeral owner', () => {
    const harness = prepareSelection({ throwFail: true });

    expect(
      harness.coordinator.settlePublicationFailure(
        harness.navigation,
        'auction-one',
        'slot-one',
        'prebid_admission_failed'
      )
    ).toBe(false);
    expect(harness.attempts[0]?.snapshot().outcome).toEqual({
      outcome: 'cancelled',
      reason: 'navigation_disposed',
    });
    expect(harness.navigation.snapshotInventoryForTest()).toMatchObject({
      attempts: 0,
      batches: 0,
    });
    harness.runtime.dispose();
  });

  it('promotes only the exact selected TS id and suppresses its group losers', () => {
    const harness = prepareSelection();
    const selected = harness.admitted('a');
    const losing = harness.admitted('b');

    harness.coordinator.auctionEnded(
      Object.freeze({ auctionId: 'auction-one' }),
      Object.freeze({
        highestBids: () =>
          Object.freeze([
            Object.freeze({
              ...selected.bid,
              adUnitCode: selected.adUnitCode,
              auctionId: selected.auctionId,
            }),
          ]),
      })
    );

    expect(harness.attempts).toHaveLength(1);
    expect(harness.promotions).toEqual([expect.objectContaining({ ok: true })]);
    expect(harness.reservations.recognize(selected.bid.adId)).toMatchObject({
      state: 'renderable',
    });
    expect(harness.reservations.recognize(losing.bid.adId)).toMatchObject({
      state: 'unselected',
    });
    expect(harness.attemptOwners[0]?.winnerContext).toEqual({ selectedCpm: 1.25 });
    expect(harness.attempts[0]?.winnerContext).toBeUndefined();
    expect(harness.activateAttempt).toHaveBeenCalledTimes(1);
    expect(harness.timers).toHaveLength(0);
    harness.runtime.dispose();
  });

  it('selects an APS reservation after Prebid strips unknown top-level fields', () => {
    // The legacy adapter carried the executable APS descriptor in a custom
    // top-level field, which Prebid normalization dropped. The hard-cutover
    // contract is stronger: only first-class `adId` plus per-bid `meta`
    // identity cross Prebid; the executable source remains in the reservation.
    const harness = prepareSelection();
    const selected = harness.admitted('m');
    const normalized = Object.freeze({
      adId: selected.bid.adId,
      adUnitCode: selected.adUnitCode,
      auctionId: selected.auctionId,
      bidderCode: selected.bid.bidderCode,
      cpm: selected.bid.cpm,
      meta: Object.freeze({ ...selected.bid.meta }),
      requestId: selected.bid.requestId,
    });

    harness.coordinator.auctionEnded(
      Object.freeze({ auctionId: selected.auctionId }),
      Object.freeze({ highestBids: () => Object.freeze([normalized]) })
    );

    expect(harness.reservations.recognize(selected.bid.adId)).toMatchObject({
      state: 'renderable',
    });
    expect(harness.activateAttempt).toHaveBeenCalledOnce();
    harness.runtime.dispose();
  });

  it('tombstones a selected reservation when its PUC attempt cannot activate', () => {
    const harness = prepareSelection({ activateResult: false });
    const selected = harness.admitted('f');

    harness.coordinator.auctionEnded(
      Object.freeze({ auctionId: 'auction-one' }),
      Object.freeze({
        highestBids: () =>
          Object.freeze([
            Object.freeze({
              ...selected.bid,
              adUnitCode: selected.adUnitCode,
              auctionId: selected.auctionId,
            }),
          ]),
      })
    );

    expect(harness.reservations.recognize(selected.bid.adId)).toMatchObject({ state: 'stale' });
    expect(harness.attempts[0]?.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'prebid_contract_violation',
    });
    harness.runtime.dispose();
  });

  it('marks the whole TS group unselected when native Prebid wins', () => {
    const harness = prepareSelection();
    const losing = harness.admitted('c');

    harness.coordinator.auctionEnded(
      Object.freeze({ auctionId: 'auction-one' }),
      Object.freeze({
        highestBids: () =>
          Object.freeze([
            Object.freeze({
              adId: 'native-prebid-id',
              adUnitCode: 'slot-one',
              auctionId: 'auction-one',
              cpm: 9,
            }),
          ]),
      })
    );

    expect(harness.reservations.recognize(losing.bid.adId)).toMatchObject({
      state: 'unselected',
    });
    expect(harness.attempts).toEqual([]);
    expect(harness.timers).toHaveLength(0);
    harness.runtime.dispose();
  });

  it('fails closed when the pinned single-unit winner query is ambiguous', () => {
    const harness = prepareSelection();
    const selected = harness.admitted('i');

    harness.coordinator.auctionEnded(
      Object.freeze({ auctionId: 'auction-one' }),
      Object.freeze({
        highestBids: () =>
          Object.freeze([
            Object.freeze({
              ...selected.bid,
              adUnitCode: selected.adUnitCode,
              auctionId: selected.auctionId,
            }),
            Object.freeze({
              adId: 'native-prebid-id',
              adUnitCode: selected.adUnitCode,
              auctionId: selected.auctionId,
              cpm: selected.bid.cpm,
            }),
          ]),
      })
    );

    expect(harness.reservations.recognize(selected.bid.adId)).toMatchObject({
      state: 'unselected',
    });
    expect(harness.attempts).toEqual([]);
    expect(harness.timers).toHaveLength(0);
    harness.runtime.dispose();
  });

  it('times out a missing auctionEnd and cancels the watchdog on navigation disposal', () => {
    const timedOut = prepareSelection();
    const bid = timedOut.admitted('d');
    timedOut.setNow(9_999);
    expect(timedOut.timers.size).toBe(1);
    [...timedOut.timers.values()][0]?.();
    expect(timedOut.reservations.recognize(bid.bid.adId)).toMatchObject({
      state: 'prebid_selection_timeout',
    });
    timedOut.runtime.dispose();

    const disposed = prepareSelection();
    const disposedBid = disposed.admitted('e');
    disposed.runtime.replaceNavigation();
    expect(disposed.reservations.recognize(disposedBid.bid.adId)).toMatchObject({
      state: 'aborted',
    });
    expect(disposed.timers).toHaveLength(0);
  });

  it('aborts every ad unit in one exact auction and releases each short lease at expiry', () => {
    const harness = prepareSelection();
    const first = harness.admitted('j', 'slot-one');
    const second = harness.admitted('k', 'slot-two');

    harness.coordinator.abort(harness.navigation, 'auction-one');

    expect(harness.reservations.recognize(first.bid.adId)).toMatchObject({ state: 'aborted' });
    expect(harness.reservations.recognize(second.bid.adId)).toMatchObject({ state: 'aborted' });
    expect(harness.timers).toHaveLength(0);
    expect(harness.navigation.snapshotInventoryForTest().batches).toBe(0);

    harness.setNow(10_000);
    expect(harness.reservations.recognize(first.bid.adId)).toEqual({ recognized: false });
    expect(harness.reservations.recognize(second.bid.adId)).toEqual({ recognized: false });
    expect(harness.reservations.snapshotInventoryForTest().size).toBe(0);
    harness.runtime.dispose();
  });

  it('selects independently across multiple ad units without promoting either group loser', () => {
    const harness = prepareSelection();
    const first = harness.admitted('l', 'slot-one');
    const firstLoser = harness.admitted('m', 'slot-one');
    const second = harness.admitted('n', 'slot-two');
    const secondLoser = harness.admitted('o', 'slot-two');

    harness.coordinator.auctionEnded(
      Object.freeze({ auctionId: 'auction-one' }),
      Object.freeze({
        highestBids: (adUnitCode?: string) => {
          const selected = adUnitCode === 'slot-one' ? first : second;
          return Object.freeze([
            Object.freeze({
              ...selected.bid,
              adUnitCode: selected.adUnitCode,
              auctionId: selected.auctionId,
            }),
          ]);
        },
      })
    );

    expect(harness.reservations.recognize(first.bid.adId)).toMatchObject({ state: 'renderable' });
    expect(harness.reservations.recognize(second.bid.adId)).toMatchObject({ state: 'renderable' });
    expect(harness.reservations.recognize(firstLoser.bid.adId)).toMatchObject({
      state: 'unselected',
    });
    expect(harness.reservations.recognize(secondLoser.bid.adId)).toMatchObject({
      state: 'unselected',
    });
    expect(harness.attempts).toHaveLength(2);
    expect(harness.timers).toHaveLength(0);
    harness.runtime.dispose();
  });

  it('rolls back a scheduler that invokes the deadline before timer publication returns', () => {
    const harness = prepareSelection({ synchronousTimer: true });
    const bid = harness.admitted('g');

    expect(harness.reservations.recognize(bid.bid.adId)).toMatchObject({
      state: 'prebid_selection_timeout',
    });
    expect(harness.timers).toHaveLength(0);
    expect(harness.navigation.snapshotInventoryForTest().batches).toBe(0);
    harness.runtime.dispose();
  });

  it.each([
    { failure: 'attempt creation', options: { throwCreateAttempt: true } },
    { failure: 'reservation promotion', options: { throwPromotion: true } },
  ])('fails closed when $failure throws during selection', ({ options }) => {
    const harness = prepareSelection(options);
    const selected = harness.admitted('h');

    expect(() =>
      harness.coordinator.auctionEnded(
        Object.freeze({ auctionId: 'auction-one' }),
        Object.freeze({
          highestBids: () =>
            Object.freeze([
              Object.freeze({
                ...selected.bid,
                adUnitCode: selected.adUnitCode,
                auctionId: selected.auctionId,
              }),
            ]),
        })
      )
    ).not.toThrow();

    expect(harness.reservations.recognize(selected.bid.adId)).toMatchObject({
      state: 'unselected',
    });
    expect(harness.attempts[0]?.snapshot().outcome).toEqual(
      options.throwPromotion
        ? { outcome: 'failed', reason: 'prebid_contract_violation' }
        : undefined
    );
    expect(harness.timers).toHaveLength(0);
    expect(harness.navigation.snapshotInventoryForTest().batches).toBe(0);
    harness.runtime.dispose();
  });
});
