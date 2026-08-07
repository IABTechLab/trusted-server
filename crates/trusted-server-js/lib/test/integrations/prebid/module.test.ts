import { describe, expect, it, vi } from 'vitest';

import { PrebidAdmissionContractError } from '../../../src/adapters/prebid';
import {
  createPrebidSelectionCoordinator,
  createPrebidIntegrationRegistration,
  publishPrebidBid,
  type PrebidBidPublicationInput,
  type PreparedTrustedBidV1,
} from '../../../src/integrations/prebid/module';
import { createTestNavigationIdentityIssuer } from '../../../src/kernel/identity';
import {
  createIntegrationRegistry,
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

describe('transactional Prebid integration module', () => {
  it('prepares inertly and starts the external boundary only after commit', async () => {
    const config = Object.freeze({ clientSideBidders: Object.freeze(['rubicon']) });
    const order: string[] = [];
    const start = vi.fn((received: unknown) => {
      order.push('start');
      expect(received).toBe(config);
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
        interfaces: Object.freeze({ prebid: Object.freeze({ start }) }),
      }),
    });
    registry.register(createPrebidIntegrationRegistration(RELEASE_ID));
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
    expect(order).toEqual(['gate:prepare', 'core', 'gate:activate', 'publish', 'start', 'drain']);
    expect(start).toHaveBeenCalledExactlyOnceWith(config);
    if (result.state === 'kernel') result.dispose();
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
    registry.register(createPrebidIntegrationRegistration(RELEASE_ID));

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
        interfaces: Object.freeze({ prebid: Object.freeze({ start }) }),
      }),
    });
    registry.register(createPrebidIntegrationRegistration(RELEASE_ID));

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
        interfaces: Object.freeze({ prebid: Object.freeze({ start }) }),
      }),
    });
    registry.register(createPrebidIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'kernel',
      runtimeFailures: [{ id: 'prebid', phase: 'after_commit' }],
    });
    expect(start).toHaveBeenCalledTimes(1);
    expect(runtimeFailures).toEqual([{ id: 'prebid', phase: 'after_commit' }]);
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
          ? (candidate as Readonly<{ type: 'aps' | 'adm' | 'cache'; version: 1 }>)
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
              ? (candidate as Readonly<{ type: 'aps' | 'adm' | 'cache'; version: 1 }>)
              : undefined,
          reservations,
        });
        if (result.ok) attempts.push(result.value);
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
