import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  publishGptWinner,
  publishInitialGptProjection,
  startGptSlotOperation,
  type GptWinnerPublicationInput,
  type GptSlotOperationInput,
} from '../../../src/integrations/gpt/module';
import { createLegacyGptRegistrationForTest as createGptIntegrationRegistration } from '../../helpers/legacy_gpt_registration';
import { createGptIntegrationRegistration as createProductionGptRegistration } from '../../../src/integrations/gpt/module';
import { createRenderRuntimeIntegrationRegistration } from '../../../src/integrations/render_runtime/module';
import type { RuntimeCapabilityV1 } from '../../../src/kernel/runtime';
import { createNoopGoogletagAdapter, type GoogletagFacade } from '../../../src/adapters/googletag';
import { isGuardInstalled, resetGuardState } from '../../../src/integrations/gpt/script_guard';
import { createTestNavigationIdentityIssuer } from '../../../src/kernel/identity';
import {
  createIntegrationRegistry,
  type IntegrationInstallCallbacks,
  type IntegrationRegistration,
} from '../../../src/kernel/integration_registry';
import { createRuntimeSession } from '../../../src/kernel/sessions';
import {
  createCommittedArtifactStore,
  createRenderAttempt,
  type CommittedRenderArtifact,
  type RenderAttempt,
} from '../../../src/services/render';
import { createReservationService } from '../../../src/services/reservations';
import type { SlotRequestOutcome } from '../../../src/services/slots';
import { createTargetingService } from '../../../src/services/targeting';

const RELEASE_ID = 'a'.repeat(64);
const RESERVATION_ID = `r1_${'a'.repeat(22)}`;

function createAttemptHarness() {
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
  if (!navigationResult.ok) throw new Error('Expected navigation creation');
  const batch = navigationResult.value.createAuctionBatch('gpt-cycle');
  if (!batch) throw new Error('Expected batch creation');
  const artifacts = createCommittedArtifactStore();
  const reservations = createReservationService({
    prepareRenderSource: (candidate) =>
      typeof candidate === 'object' &&
      candidate !== null &&
      Object.isFrozen(candidate) &&
      'type' in candidate &&
      'version' in candidate
        ? (candidate as Readonly<{ type: 'aps' | 'adm' | 'cache'; version: 1 }>)
        : undefined,
  });
  const createAttemptWithOwner = (parentAttemptId?: string) => {
    const owner = batch.createRenderAttempt('slot-one');
    if (!owner.ok) throw new Error(`Expected attempt owner: ${owner.reason}`);
    const created = createRenderAttempt({
      artifacts,
      owner: owner.value,
      prepareRenderSource: (candidate) =>
        typeof candidate === 'object' &&
        candidate !== null &&
        Object.isFrozen(candidate) &&
        'type' in candidate &&
        'version' in candidate
          ? (candidate as Readonly<{ type: 'aps' | 'adm' | 'cache'; version: 1 }>)
          : undefined,
      reservations,
      ...(parentAttemptId === undefined ? {} : { parentAttemptId }),
    });
    if (!created.ok) throw new Error(`Expected render attempt: ${created.reason}`);
    return { attempt: created.value, owner: owner.value };
  };
  const primaryCreated = createAttemptWithOwner();
  const primary = primaryCreated.attempt;
  const artifact = Object.freeze({
    kind: 'puc' as const,
    attemptId: primary.id,
    slot: primary.slot,
    navigationGeneration: primary.navigationGeneration,
    dispose: vi.fn(),
  }) satisfies CommittedRenderArtifact;
  return {
    artifact,
    createAttempt: (parentAttemptId: string): RenderAttempt =>
      createAttemptWithOwner(parentAttemptId).attempt,
    navigation: navigationResult.value,
    primary,
    primaryOwner: primaryCreated.owner,
    reservations,
    runtime,
  };
}

function deferredSlotOutcome() {
  let resolve!: (outcome: SlotRequestOutcome) => void;
  const result = new Promise<SlotRequestOutcome>((resolveResult) => {
    resolve = resolveResult;
  });
  const dispose = vi.fn();
  return {
    dispose,
    request: vi.fn(() => Object.freeze({ status: 'active' as const, result, dispose })),
    resolve,
  };
}

function manifest(ids: readonly string[]) {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    criticalSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
    integrations: ids.map((id) => ({ id, phase: 'critical' as const })),
  };
}

function registration(
  id: string,
  prepare: IntegrationRegistration['prepare']
): IntegrationRegistration {
  return Object.freeze({ abi: 1, id, phase: 'critical', releaseId: RELEASE_ID, prepare });
}

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

describe('transactional GPT integration module', () => {
  afterEach(() => {
    resetGuardState();
    delete (window as Window & { googletag?: unknown }).googletag;
  });

  it.each([false, true])(
    'uses only catalog capabilities and conditions diagnostics-only GPT listeners (active=%s)',
    async (diagnosticsActive) => {
      const listenerTypes: string[] = [];
      const removedTypes: string[] = [];
      const pubads = {
        addEventListener: vi.fn((type: string, _listener: (event: unknown) => void) => {
          listenerTypes.push(type);
        }),
        disableInitialLoad: vi.fn(),
        getSlots: vi.fn(() => []),
        refresh: vi.fn(),
        removeEventListener: vi.fn((type: string, _listener: (event: unknown) => void) => {
          removedTypes.push(type);
        }),
      };
      (window as Window & { googletag?: unknown }).googletag = {
        apiReady: true,
        pubadsReady: true,
        cmd: { push: (command: () => void) => (command(), 0) },
        defineSlot: vi.fn(),
        destroySlots: vi.fn(() => true),
        display: vi.fn(),
        getConfig: vi.fn(() => ({ disableInitialLoad: false })),
        pubads: () => pubads,
        setConfig: vi.fn(),
      };
      const providerFacades = new Map<string, Readonly<Record<string, unknown>>>();
      const protect = vi.fn(() => true);
      const bootManifest = Object.freeze({
        version: 1 as const,
        releaseId: RELEASE_ID,
        criticalSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
        integrations: Object.freeze([
          Object.freeze({ id: 'render_runtime', phase: 'critical' as const }),
          Object.freeze({ id: 'gpt', phase: 'critical' as const }),
        ]),
      });
      const runtime = Object.freeze({
        boot: () =>
          Object.freeze({
            auctionProjection: Object.freeze({
              version: 1,
              auction: Object.freeze({
                version: 1,
                auctionId: 'initial',
                results: Object.freeze([]),
              }),
              slots: Object.freeze([]),
              bids: Object.freeze([]),
            }),
            diagnostics: Object.freeze({
              version: 1,
              renderTraceOverlay: false,
              gpt: Object.freeze({ active: diagnosticsActive }),
            }),
            manifest: bootManifest,
          }),
        document,
        generation: Object.freeze({}),
        protectFirstDisplayAttemptBatch: protect,
      } satisfies RuntimeCapabilityV1);
      const registry = createIntegrationRegistry({
        manifest: bootManifest,
        releaseId: RELEASE_ID,
        knownIntegrationIds: Object.freeze(['render_runtime', 'gpt']),
        catalog: Object.freeze([
          Object.freeze({
            id: 'render_runtime',
            phase: 'critical' as const,
            trigger: null,
            consumes: Object.freeze(['runtime.v1']),
            provides: Object.freeze([
              'slots.v1',
              'auction.v1',
              'render.v1',
              'messages.v1',
              'trace.v1',
              'trace.presentation.v1',
              'direct.v1',
            ]),
          }),
          Object.freeze({
            id: 'gpt',
            phase: 'critical' as const,
            trigger: null,
            consumes: Object.freeze([
              'runtime.v1',
              'slots.v1',
              'auction.v1',
              'render.v1',
              'messages.v1',
              'trace.v1',
            ]),
            provides: Object.freeze(['gpt.v1', 'gpt.events.v1', 'pbs_cache.baseline.v1']),
          }),
        ]),
        runtimeCapability: runtime,
        getBindings: (id) =>
          Object.freeze({
            config: id === 'gpt' ? Object.freeze({}) : undefined,
            interfaces: Object.freeze({}),
          }),
        onCapabilityStaged: (key, facade) => {
          providerFacades.set(key, facade);
          return () => {
            if (providerFacades.get(key) === facade) providerFacades.delete(key);
          };
        },
        startedAtMs: 0,
        now: () => 0,
      });
      expect(registry.register(createRenderRuntimeIntegrationRegistration(RELEASE_ID))).toBe(true);
      expect(registry.register(createProductionGptRegistration(RELEASE_ID))).toBe(true);

      const result = await registry.install(callbacks([]));
      expect(result.state).toBe('kernel');
      expect([...providerFacades.keys()]).toEqual([
        'slots.v1',
        'auction.v1',
        'render.v1',
        'messages.v1',
        'trace.v1',
        'trace.presentation.v1',
        'direct.v1',
        'gpt.v1',
        'gpt.events.v1',
        'pbs_cache.baseline.v1',
      ]);
      expect(Reflect.ownKeys(providerFacades.get('pbs_cache.baseline.v1') ?? {})).toEqual([
        'render',
      ]);
      const render = providerFacades.get('render.v1') as {
        registerRenderer: (type: 'cache', renderer: () => boolean) => () => void;
      };
      expect(() => render.registerRenderer('cache', () => false)).toThrow('duplicated');
      expect(protect).not.toHaveBeenCalled();
      expect([...listenerTypes].sort()).toEqual(
        (diagnosticsActive
          ? [
              'slotRequested',
              'slotRenderEnded',
              'slotResponseReceived',
              'slotOnload',
              'impressionViewable',
              'slotVisibilityChanged',
            ]
          : ['slotRequested', 'slotRenderEnded']
        ).sort()
      );
      expect(listenerTypes.slice(0, 2)).toEqual(['slotRequested', 'slotRenderEnded']);

      if (result.state === 'kernel') result.dispose();
      expect(removedTypes.sort()).toEqual([...listenerTypes].sort());
      expect(providerFacades.size).toBe(0);
      expect(() => render.registerRenderer('cache', () => false)).toThrow('inactive');
    }
  );

  it('protects the complete immutable initial winner batch before starting either GPT request', async () => {
    const candidateIds = ['candidate001', 'candidate002'] as const;
    const projection = Object.freeze({
      version: 1 as const,
      auction: Object.freeze({
        version: 1 as const,
        auctionId: 'initial-winners',
        results: Object.freeze(
          candidateIds.map((candidateId, index) =>
            Object.freeze({
              slot: `slot-${index + 1}`,
              outcome: 'winner' as const,
              candidateId,
            })
          )
        ),
      }),
      slots: Object.freeze(
        candidateIds.map((_candidateId, index) =>
          Object.freeze({
            slot: `slot-${index + 1}`,
            gamUnitPath: `/123/slot-${index + 1}`,
            divId: `slot-${index + 1}`,
            formats: Object.freeze([Object.freeze([300, 250] as const)]),
            targeting: Object.freeze({}),
          })
        )
      ),
      bids: Object.freeze(
        candidateIds.map((candidateId, index) =>
          Object.freeze({
            candidateId,
            slot: `slot-${index + 1}`,
            provider: 'fictional',
            upstreamBidId: `upstream-${index + 1}`,
            cpm: index + 1,
            currency: 'USD' as const,
            targeting: Object.freeze({}),
            rendererReservationId: `r1_${String(index + 1).repeat(22)}`,
            renderSource: Object.freeze({
              type: 'adm' as const,
              version: 1 as const,
              adm: `<p>${index + 1}</p>`,
              width: 300,
              height: 250,
            }),
          })
        )
      ),
    });
    for (const placement of projection.slots) {
      const element = document.createElement('div');
      element.id = placement.divId;
      document.body.appendChild(element);
    }
    const runtime = createRuntimeSession({
      createIdentityIssuer: () =>
        createTestNavigationIdentityIssuer({
          getRandomValues: (target) => {
            target.fill(7);
            return target;
          },
        }),
    });
    const navigationResult = runtime.startInitialNavigation(projection);
    if (!navigationResult.ok) throw new Error(navigationResult.reason);
    const navigation = navigationResult.value;
    const artifacts = createCommittedArtifactStore();
    const reservations = createReservationService({
      prepareRenderSource: (candidate) =>
        typeof candidate === 'object' && candidate !== null && Object.isFrozen(candidate)
          ? (candidate as never)
          : undefined,
    });
    const physical = new Map<string, object>();
    const request = vi.fn(() =>
      Object.freeze({
        status: 'active' as const,
        result: Promise.resolve(
          Object.freeze({ status: 'failed' as const, reason: 'gpt_request_failed' as const })
        ),
        dispose: vi.fn(),
      })
    );
    const slots = Object.freeze({
      adoptGptSlot: (
        _generation: object,
        registeredSlotId: string,
        binding: Readonly<{ slot: object }>
      ) => {
        physical.set(registeredSlotId, binding.slot);
        return Object.freeze({ ok: true as const });
      },
      isBoundGptSlot: (_generation: object, registeredSlotId: string, slot: object) =>
        physical.get(registeredSlotId) === slot,
      recordPublisherDestruction: vi.fn(() => true),
      request,
    });
    const targeting = Object.freeze({
      observePublisherMutations: () =>
        Object.freeze({ status: 'completed', result: Promise.resolve(), dispose: vi.fn() }),
      own: (_slot: object, _key: string, _value: string, ownerId: string) =>
        Object.freeze({ ownerId, release: vi.fn() }),
    });
    const facade = Object.freeze({
      slots: () => Object.freeze([]),
      slotElementId: () => undefined,
      transactionalDefine: (
        definition: Readonly<{ elementId: string }>,
        _current: () => boolean,
        prepare: (slot: object) => Readonly<{ commit: () => boolean }>
      ) => {
        const slot = Object.freeze({ elementId: definition.elementId });
        if (!prepare(slot).commit()) return Object.freeze({ status: 'failed' as const });
        return Object.freeze({ status: 'defined' as const, slot });
      },
      clearTargeting: vi.fn(),
      getTargeting: vi.fn(() => Object.freeze([])),
      setTargeting: vi.fn(),
    });
    const googletag = Object.freeze({
      run: (command: (gpt: typeof facade) => unknown) =>
        Object.freeze({
          status: 'completed',
          result: Promise.resolve(command(facade)),
          dispose: vi.fn(),
        }),
    });
    let protectedLatches: readonly PromiseLike<unknown>[] | undefined;
    const protect = vi.fn((latches: readonly PromiseLike<unknown>[]) => {
      expect(Object.isFrozen(latches)).toBe(true);
      expect(latches).toHaveLength(2);
      expect(request).not.toHaveBeenCalled();
      protectedLatches = latches;
      return true;
    });

    await publishInitialGptProjection(document, {
      googletag: googletag as never,
      navigation,
      projection: projection as never,
      protect,
      pucBridge: Object.freeze({
        registerGamAttempt: vi.fn(() => true),
        recordNonemptyGam: vi.fn(() => true),
      }),
      render: Object.freeze({
        artifacts,
        createAttempt: (owner: Parameters<typeof createRenderAttempt>[0]['owner']) =>
          createRenderAttempt({
            artifacts,
            owner,
            prepareRenderSource: (candidate) => candidate as never,
            reservations,
          }),
        publisherOrigin: window.location.origin,
        registerRenderer: vi.fn(),
        rendererNonces: Object.freeze({}),
        renderWinner: vi.fn(() => false),
        reservations,
      }) as never,
      slots: slots as never,
      targeting: targeting as never,
    });

    expect(protect).toHaveBeenCalledOnce();
    expect(request).toHaveBeenCalledTimes(2);
    await Promise.allSettled([...(protectedLatches ?? [])]);
    artifacts.dispose();
    reservations.dispose();
    runtime.dispose();
  });

  it('prepares inertly, activates the reversible guard, and starts only after commit', async () => {
    const config = Object.freeze({ scriptUrl: '/integrations/gpt/script' });
    const order: string[] = [];
    const start = vi.fn((received: unknown) => {
      order.push('start');
      expect(received).toBe(config);
    });
    const release = vi.fn(() => order.push('release'));
    const activate = vi.fn(() => {
      order.push('gpt:activate');
      return release;
    });
    let finishPreparation: (() => void) | undefined;
    const preparationGate = new Promise<void>((resolve) => {
      finishPreparation = resolve;
    });
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'gate']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt', 'gate']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({ gpt: Object.freeze({ activate, start }) }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));
    registry.register(
      registration('gate', async () => {
        order.push('gate:prepare');
        await preparationGate;
        return Object.freeze({ activate: () => order.push('gate:activate') });
      })
    );
    const originalDocumentWrite = document.write;

    const installing = registry.install(callbacks(order));
    await vi.waitFor(() => expect(order).toEqual(['gate:prepare']));

    expect(isGuardInstalled()).toBe(false);
    expect(document.write).toBe(originalDocumentWrite);
    expect(start).not.toHaveBeenCalled();

    finishPreparation?.();
    const result = await installing;

    expect(result).toMatchObject({ state: 'kernel' });
    expect(isGuardInstalled()).toBe(true);
    expect(document.write).not.toBe(originalDocumentWrite);
    expect(order).toEqual([
      'gate:prepare',
      'core',
      'gpt:activate',
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
    expect(isGuardInstalled()).toBe(false);
    expect(document.write).toBe(originalDocumentWrite);
    expect(release).toHaveBeenCalledTimes(1);
  });

  it('unwinds the GPT guard before fallback when a later activation fails', async () => {
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'broken']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt', 'broken']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({}),
        interfaces: Object.freeze({
          gpt: Object.freeze({ activate: () => vi.fn(), start }),
        }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));
    registry.register(
      registration('broken', () => ({
        activate: () => {
          expect(isGuardInstalled()).toBe(true);
          throw new Error('fictional activation failure');
        },
      }))
    );

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });

    expect(isGuardInstalled()).toBe(false);
    expect(start).not.toHaveBeenCalled();
  });

  it('never installs the guard or starts when reversible GPT activation fails', async () => {
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({}),
        interfaces: Object.freeze({
          gpt: Object.freeze({
            activate: () => {
              expect(isGuardInstalled()).toBe(false);
              throw new Error('fictional observer activation failure');
            },
            start,
          }),
        }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(isGuardInstalled()).toBe(false);
    expect(start).not.toHaveBeenCalled();
  });

  it('fails preparation without effects when the composition omits the GPT boundary', async () => {
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });

    expect(isGuardInstalled()).toBe(false);
  });

  it.each([
    [
      'accessor',
      Object.freeze(
        Object.defineProperty({}, 'scriptUrl', {
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
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({
          gpt: Object.freeze({ activate: () => vi.fn(), start }),
        }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(start).not.toHaveBeenCalled();
    expect(isGuardInstalled()).toBe(false);
  });

  it('isolates post-commit startup failure and disposes only the GPT module', async () => {
    const start = vi.fn(() => {
      throw new Error('fictional GPT startup failure');
    });
    const runtimeFailures: unknown[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
      onRuntimeFailure: (failure) => runtimeFailures.push(failure),
      getBindings: () => ({
        config: Object.freeze({}),
        interfaces: Object.freeze({
          gpt: Object.freeze({ activate: () => vi.fn(), start }),
        }),
      }),
    });
    registry.register(createGptIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'kernel',
      runtimeFailures: [{ id: 'gpt', phase: 'after_commit' }],
    });

    expect(start).toHaveBeenCalledTimes(1);
    expect(runtimeFailures).toEqual([{ id: 'gpt', phase: 'after_commit' }]);
    expect(isGuardInstalled()).toBe(false);
  });

  it('starts fallback only after an attributable TS-owned empty cycle settles the primary', async () => {
    const harness = createAttemptHarness();
    const slot = deferredSlotOutcome();
    const order: string[] = [];
    let fallback: RenderAttempt | undefined;
    const bridgeInput: unknown[] = [];
    const bridge = {
      registerGamAttempt: vi.fn((input: GptSlotOperationInput) => {
        bridgeInput.push(input);
        return input.attempt.beginGamClaim();
      }),
      recordNonemptyGam: vi.fn(() => true),
    };
    const started = startGptSlotOperation({
      artifact: harness.artifact,
      attempt: harness.primary,
      createFallback: (parentAttemptId) => {
        expect(harness.primary.snapshot().outcome).toEqual({
          outcome: 'failed',
          reason: 'gam_empty',
        });
        order.push('fallback:create');
        fallback = harness.createAttempt(parentAttemptId);
        return Object.freeze({ ok: true as const, value: fallback });
      },
      operation: 'refresh',
      owner: harness.primaryOwner,
      pucBridge: bridge,
      requestClass: 'primary',
      reservationId: RESERVATION_ID,
      slots: { request: slot.request },
    });

    expect(started.ok).toBe(true);
    expect(bridge.registerGamAttempt).toHaveBeenCalledTimes(1);
    expect(slot.request).toHaveBeenCalledWith({
      intentId: harness.primary.id,
      navigationGeneration: harness.primary.navigationGeneration,
      operation: 'refresh',
      registeredSlotId: harness.primary.slot,
      requestClass: 'primary',
    });

    slot.resolve(Object.freeze({ status: 'empty', responseIdentifier: 'response-one' }));
    await Promise.resolve();

    expect(order).toEqual(['fallback:create']);
    expect(harness.primary.snapshot()).toMatchObject({
      state: 'failed',
      outcome: { outcome: 'failed', reason: 'gam_empty' },
    });
    expect(started.ok && started.value.snapshot()).toEqual({ settled: false });
    expect(slot.dispose).toHaveBeenCalledTimes(1);
    expect(bridge.recordNonemptyGam).not.toHaveBeenCalled();

    expect(fallback?.fail('gpt_request_failed')).toBe(true);
    expect(started.ok && started.value.snapshot()).toMatchObject({
      settled: true,
      result: {
        path: 'fallback',
        primaryAttemptId: harness.primary.id,
        primary: { outcome: 'failed', reason: 'gam_empty' },
        fallbackAttemptId: fallback?.id,
        fallback: { outcome: 'failed', reason: 'gpt_request_failed' },
      },
    });
    expect(harness.primary.snapshot().outcome).toEqual({
      outcome: 'failed',
      reason: 'gam_empty',
    });
    harness.runtime.dispose();
  });

  it('joins an attributable nonempty cycle to the PUC bridge without settling the operation', async () => {
    const harness = createAttemptHarness();
    const slot = deferredSlotOutcome();
    const registered: unknown[] = [];
    const nonempty: unknown[] = [];
    const bridge = {
      registerGamAttempt: vi.fn((input: GptSlotOperationInput) => {
        registered.push(input);
        return input.attempt.beginGamClaim();
      }),
      recordNonemptyGam: vi.fn((input: unknown) => {
        nonempty.push(input);
        return true;
      }),
    };
    const input = {
      artifact: harness.artifact,
      attempt: harness.primary,
      operation: 'display' as const,
      owner: harness.primaryOwner,
      pucBridge: bridge,
      requestClass: 'primary',
      reservationId: RESERVATION_ID,
      slots: { request: slot.request },
    };
    const started = startGptSlotOperation(input);
    slot.resolve(Object.freeze({ status: 'rendered', responseIdentifier: 'response-one' }));
    await Promise.resolve();

    expect(nonempty).toEqual(registered);
    expect(started.ok && started.value.snapshot()).toEqual({ settled: false });
    expect(harness.primary.snapshot().state).toBe('waiting_for_gam_and_claim');
    expect(slot.dispose).not.toHaveBeenCalled();

    harness.primary.cancel('superseded');
    expect(slot.dispose).toHaveBeenCalledTimes(1);
    harness.runtime.dispose();
  });

  it.each([
    [{ status: 'failed', reason: 'cycle_unattributable' }, 'cycle_unattributable'],
    [{ status: 'failed', reason: 'slot_quarantined' }, 'slot_quarantined'],
    [{ status: 'failed', reason: 'gpt_request_timeout' }, 'gpt_request_timeout'],
    [{ status: 'failed', reason: 'gpt_completion_timeout' }, 'gpt_completion_timeout'],
    [{ status: 'failed', reason: 'external_queue_full' }, 'external_queue_full'],
    [{ status: 'failed', reason: 'external_ready_timeout' }, 'external_ready_timeout'],
    [{ status: 'cancelled', reason: 'navigation_disposed' }, 'navigation_disposed'],
  ] as const)(
    'does not start fallback for non-empty terminal cycle outcome %s',
    async (slotOutcome, reason) => {
      const harness = createAttemptHarness();
      const slot = deferredSlotOutcome();
      const createFallback = vi.fn();
      const started = startGptSlotOperation({
        artifact: harness.artifact,
        attempt: harness.primary,
        createFallback,
        operation: 'refresh',
        owner: harness.primaryOwner,
        pucBridge: {
          registerGamAttempt: (input) => input.attempt.beginGamClaim(),
          recordNonemptyGam: () => true,
        },
        requestClass: 'primary',
        reservationId: RESERVATION_ID,
        slots: { request: slot.request },
      });
      slot.resolve(Object.freeze(slotOutcome) as SlotRequestOutcome);
      await Promise.resolve();

      expect(createFallback).not.toHaveBeenCalled();
      expect(started.ok && started.value.snapshot()).toMatchObject({
        settled: true,
        result: {
          path: 'primary',
          outcome: { reason },
        },
      });
      harness.runtime.dispose();
    }
  );
});

describe('ordered GPT winner publication', () => {
  function preparePublication() {
    const harness = createAttemptHarness();
    const source = Object.freeze({
      type: 'adm' as const,
      version: 1 as const,
      adm: '<main>trusted</main>',
      width: 300,
      height: 250,
    });
    const bid = Object.freeze({
      candidateId: 'AAAAAAAAAAAA',
      slot: harness.primary.slot,
      provider: 'trusted',
      upstreamBidId: 'upstream-one',
      cpm: 1.25,
      currency: 'USD' as const,
      targeting: Object.freeze({ hb_bidder: 'trusted' }),
      rendererReservationId: RESERVATION_ID,
      renderSource: source,
    });
    const placement = Object.freeze({
      slot: bid.slot,
      gamUnitPath: '/123/gpt-slot',
      divId: 'gpt-slot',
      formats: Object.freeze([Object.freeze([300, 250] as const)]),
      targeting: Object.freeze({ hb_bidder: 'publisher', pos: 'top' }),
    });
    const projection = Object.freeze({
      version: 1,
      auction: Object.freeze({
        version: 1,
        auctionId: 'gpt-publication',
        results: Object.freeze([
          Object.freeze({
            slot: bid.slot,
            outcome: 'winner' as const,
            candidateId: bid.candidateId,
          }),
        ]),
      }),
      slots: Object.freeze([placement]),
      bids: Object.freeze([bid]),
    });
    expect(harness.navigation.installAuctionProjection(projection)).toBe(true);

    const order: string[] = [];
    const values = new Map<string, readonly string[]>();
    const slot = Object.freeze({
      clearTargeting: vi.fn((key?: string) => {
        if (key === undefined) values.clear();
        else values.delete(key);
      }),
      getTargeting: vi.fn((key: string) => Object.freeze([...(values.get(key) ?? [])])),
      setTargeting: vi.fn((key: string, value: string | readonly string[]) => {
        order.push(`target:${key}`);
        values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
      }),
    });
    const facade: GoogletagFacade = Object.freeze({
      bindingToken: () => Object.freeze({}),
      clearTargeting: (target: object, key?: string) => (target as typeof slot).clearTargeting(key),
      transactionalDefine: () => Object.freeze({ status: 'discarded' as const }),
      display: vi.fn(),
      getTargeting: (target: object, key: string) => (target as typeof slot).getTargeting(key),
      observeTargeting: () => {
        order.push('observe');
        return Object.assign(vi.fn(), { isCurrent: () => true });
      },
      refresh: vi.fn(),
      serviceState: () =>
        Object.freeze({ apiReady: true, initialLoadDisabled: false, pubadsReady: true }),
      setTargeting: (target: object, key: string, value: string | readonly string[]) =>
        (target as typeof slot).setTargeting(key, value),
      slotElementId: () => undefined,
      slots: () => Object.freeze([slot]),
      subscribe: () => vi.fn(),
      transactionalReplace: () => Object.freeze({ status: 'destroyed' as const }),
    });
    const googletag = Object.freeze({
      ...createNoopGoogletagAdapter(),
      bindingStatus: () => 'present' as const,
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
    const targeting = createTargetingService();
    const slotOutcome = deferredSlotOutcome();
    const slots = {
      isBoundGptSlot: vi.fn(() => {
        order.push('slot:validate');
        return true;
      }),
      request: vi.fn((input: unknown) => {
        order.push('request');
        expect(input).toMatchObject({ registeredSlotId: bid.slot });
        expect(harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
          recognized: true,
          state: 'renderable',
        });
        return slotOutcome.request();
      }),
    };
    let bridgeArtifact: CommittedRenderArtifact | undefined;
    const pucBridge = {
      registerGamAttempt: vi.fn((input: GptSlotOperationInput) => {
        order.push('bridge');
        bridgeArtifact = input.artifact;
        return input.attempt.beginGamClaim();
      }),
      recordNonemptyGam: vi.fn(() => true),
    };
    const reservations = {
      registerRender: vi.fn((input: Parameters<typeof harness.reservations.registerRender>[0]) => {
        order.push('reservation');
        return harness.reservations.registerRender(input);
      }),
      tombstone: harness.reservations.tombstone,
    };
    const input: GptWinnerPublicationInput = {
      artifact: harness.artifact,
      attempt: harness.primary,
      bid,
      googletag,
      navigation: harness.navigation,
      operation: 'refresh',
      owner: harness.primaryOwner,
      placement,
      pucBridge,
      requestClass: 'primary',
      reservations,
      slot,
      slots,
      targeting,
    };
    return {
      bid,
      bridgeArtifact: () => bridgeArtifact,
      harness,
      input,
      order,
      pucBridge,
      reservations,
      slot,
      slots,
      targeting,
      values,
    };
  }

  it('publishes reservation, targeting, intent, and request in that exact order', async () => {
    const publication = preparePublication();

    const result = await publishGptWinner(publication.input);

    expect(result.ok).toBe(true);
    expect(publication.order).toEqual([
      'slot:validate',
      'reservation',
      'observe',
      'slot:validate',
      'target:hb_adid',
      'target:hb_bidder',
      'target:pos',
      'slot:validate',
      'bridge',
      'request',
    ]);
    expect(publication.values).toEqual(
      new Map([
        ['hb_adid', [RESERVATION_ID]],
        ['hb_bidder', ['trusted']],
        ['pos', ['top']],
      ])
    );
    publication.bridgeArtifact()?.dispose();
    expect(publication.values.size).toBe(0);
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    expect(publication.harness.artifact.dispose).toHaveBeenCalledTimes(1);
    publication.harness.runtime.dispose();
  });

  it('fails before targeting when exact slot ownership is lost across observation', async () => {
    const publication = preparePublication();
    publication.slots.isBoundGptSlot
      .mockImplementationOnce(() => {
        publication.order.push('slot:validate');
        return true;
      })
      .mockImplementation(() => {
        publication.order.push('slot:validate');
        return false;
      });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'slot_unresolved',
    });
    expect(publication.order).toEqual(['slot:validate', 'reservation', 'observe', 'slot:validate']);
    expect(publication.values.size).toBe(0);
    expect(publication.slots.request).not.toHaveBeenCalled();
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    expect(publication.harness.artifact.dispose).toHaveBeenCalledTimes(1);
    publication.harness.runtime.dispose();
  });

  it('compare-restores targeting when exact slot ownership is lost during writes', async () => {
    const publication = preparePublication();
    publication.slots.isBoundGptSlot
      .mockImplementationOnce(() => {
        publication.order.push('slot:validate');
        return true;
      })
      .mockImplementationOnce(() => {
        publication.order.push('slot:validate');
        return true;
      })
      .mockImplementation(() => {
        publication.order.push('slot:validate');
        return false;
      });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'slot_unresolved',
    });
    expect(publication.order).toEqual([
      'slot:validate',
      'reservation',
      'observe',
      'slot:validate',
      'target:hb_adid',
      'target:hb_bidder',
      'target:pos',
      'slot:validate',
    ]);
    expect(publication.values.size).toBe(0);
    expect(publication.slots.request).not.toHaveBeenCalled();
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    publication.harness.runtime.dispose();
  });

  it('rolls back targeting and tombstones when the bridge refuses before request', async () => {
    const publication = preparePublication();
    publication.pucBridge.registerGamAttempt.mockImplementation(() => {
      publication.order.push('bridge');
      return false;
    });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'gpt_request_failed',
    });
    expect(publication.slots.request).not.toHaveBeenCalled();
    expect(publication.values.size).toBe(0);
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    expect(publication.harness.artifact.dispose).toHaveBeenCalledTimes(1);
    publication.harness.runtime.dispose();
  });

  it('rolls back targeting and tombstones when the slot request throws', async () => {
    const publication = preparePublication();
    publication.slots.request.mockImplementation(() => {
      publication.order.push('request');
      throw new Error('fictional request failure');
    });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'gpt_request_failed',
    });
    expect(publication.order).toEqual([
      'slot:validate',
      'reservation',
      'observe',
      'slot:validate',
      'target:hb_adid',
      'target:hb_bidder',
      'target:pos',
      'slot:validate',
      'bridge',
      'request',
    ]);
    expect(publication.values.size).toBe(0);
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    expect(publication.harness.artifact.dispose).toHaveBeenCalledTimes(1);
    publication.harness.runtime.dispose();
  });

  it('fails before exposure when reservation insertion collides', async () => {
    const publication = preparePublication();
    expect(
      publication.harness.reservations.registerRender({
        reservationId: RESERVATION_ID,
        slot: publication.bid.slot,
        navigation: publication.harness.navigation,
        attemptId: publication.harness.primary.id,
        renderSource: publication.bid.renderSource,
        winnerContext: Object.freeze({ selectedCpm: publication.bid.cpm }),
      })
    ).toMatchObject({ ok: true });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'reservation_collision',
    });
    expect(publication.order).toEqual(['slot:validate', 'reservation']);
    expect(publication.values.size).toBe(0);
    expect(publication.pucBridge.registerGamAttempt).not.toHaveBeenCalled();
    expect(publication.harness.artifact.dispose).toHaveBeenCalledTimes(1);
    publication.harness.runtime.dispose();
  });

  it('compare-restores earlier targeting when a later targeting write throws', async () => {
    const publication = preparePublication();
    publication.slot.setTargeting.mockImplementation((key, value) => {
      publication.order.push(`target:${key}`);
      if (key === 'hb_bidder') throw new Error('fictional targeting failure');
      publication.values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
    });

    await expect(publishGptWinner(publication.input)).resolves.toEqual({
      ok: false,
      reason: 'gpt_request_failed',
    });
    expect(publication.values.size).toBe(0);
    expect(publication.slots.request).not.toHaveBeenCalled();
    expect(publication.harness.reservations.recognize(RESERVATION_ID)).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    publication.harness.runtime.dispose();
  });
});
