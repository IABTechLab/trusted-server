import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  createGptIntegrationRegistration,
  publishGptWinner,
  startGptSlotOperation,
  type GptWinnerPublicationInput,
  type GptSlotOperationInput,
} from '../../../src/integrations/gpt/module';
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
    integrations: ids.map((id) => ({ id, required: true })),
  };
}

function registration(
  id: string,
  prepare: IntegrationRegistration['prepare']
): IntegrationRegistration {
  return Object.freeze({ id, release: RELEASE_ID, prepare });
}

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

describe('transactional GPT integration module', () => {
  afterEach(() => resetGuardState());

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
      display: vi.fn(),
      getTargeting: (target: object, key: string) => (target as typeof slot).getTargeting(key),
      observeTargeting: () => {
        order.push('observe');
        return vi.fn();
      },
      refresh: vi.fn(),
      serviceState: () =>
        Object.freeze({ apiReady: true, initialLoadDisabled: false, pubadsReady: true }),
      setTargeting: (target: object, key: string, value: string | readonly string[]) =>
        (target as typeof slot).setTargeting(key, value),
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
      'slot:validate',
      'bridge',
      'request',
    ]);
    expect(publication.values).toEqual(
      new Map([
        ['hb_adid', [RESERVATION_ID]],
        ['hb_bidder', ['trusted']],
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
