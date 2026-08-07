import { describe, expect, it, vi } from 'vitest';

import {
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
import { createRuntimeSession } from '../../../src/kernel/sessions';
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
    };
  }

  it('registers the lease before exposing one capability-free frozen bid', () => {
    const publication = preparePublication();

    const result = publishPrebidBid(publication.input);

    expect(result.ok).toBe(true);
    expect(publication.order).toEqual(['reservation', 'admit']);
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
