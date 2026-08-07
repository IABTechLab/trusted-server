import { describe, expect, it, vi } from 'vitest';

import { parseCacheFetchPolicyV1 } from '../../src/core/config';
import { parseBidRenderSourceV1 } from '../../src/core/contracts/auction_projection';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';
import {
  createRuntimeSession,
  type NavigationSession,
  type RenderAttemptScope,
  type WinnerContext,
} from '../../src/kernel/sessions';
import {
  PREBID_ADMISSION_LEASE_MS,
  RENDER_RESERVATION_LIFETIME_MS,
  createReservationService,
  isRendererReservationId,
  type ReservationOwner,
} from '../../src/services/reservations';

const CACHE_ID = '123e4567-e89b-42d3-a456-426614174000';

function reservationId(index = 0): string {
  return `r1_${index.toString(36).padStart(22, '0')}`;
}

function runtimeNavigation(): {
  readonly navigation: NavigationSession;
  readonly runtime: ReturnType<typeof createRuntimeSession>;
} {
  const runtime = createRuntimeSession({
    createIdentityIssuer: () =>
      createTestNavigationIdentityIssuer({
        getRandomValues: (target) => {
          target.fill(7);
          return target;
        },
      }),
  });
  const navigation = runtime.startInitialNavigation();
  if (!navigation.ok) throw new Error('Expected a navigation');
  return { navigation: navigation.value, runtime };
}

function renderAttempt(navigation: NavigationSession, slot = 'fictional-slot'): RenderAttemptScope {
  const batch = navigation.createAuctionBatch(`batch-${slot}`);
  if (!batch) throw new Error('Expected an auction batch');
  const attempt = batch.createRenderAttempt(slot);
  if (!attempt.ok) throw new Error('Expected a render attempt');
  return attempt.value;
}

function admSource(markup = '<div>fictional creative</div>') {
  return { type: 'adm', version: 1, adm: markup, width: 300, height: 250 } as const;
}

function cacheSource() {
  return {
    type: 'cache',
    version: 1,
    cacheId: CACHE_ID,
    fetchUrl: `https://cache.example/render?uuid=${CACHE_ID}`,
    width: 300,
    height: 250,
  } as const;
}

function apsSource() {
  const creativeUrl = 'https://creative.example/render';
  const envelope = {
    seatbid: [
      {
        bid: [
          {
            id: 'upstream-bid',
            w: 300,
            h: 250,
            price: 1.25,
            ext: { creativeurl: creativeUrl, tagtype: 'iframe' },
          },
        ],
      },
    ],
  };
  return {
    type: 'aps',
    version: 1,
    accountId: 'fictional-account',
    bidId: 'upstream-bid',
    creativeId: 'fictional-creative',
    tagType: 'iframe',
    creativeUrl,
    aaxResponse: btoa(JSON.stringify(envelope)),
    width: 300,
    height: 250,
  } as const;
}

function serviceAt(readNow: () => number) {
  const cachePolicy = parseCacheFetchPolicyV1({
    version: 1,
    baseUrl: 'https://cache.example/render',
  });
  if (!cachePolicy) throw new Error('Expected cache policy');
  return createReservationService({
    now: readNow,
    prepareRenderSource: (candidate) => parseBidRenderSourceV1(candidate, cachePolicy),
  });
}

function registerRender(
  service: ReturnType<typeof createReservationService>,
  navigation: NavigationSession,
  attempt: RenderAttemptScope,
  id = reservationId(),
  renderSource: unknown = admSource(),
  selectedCpm = 1.25
) {
  return service.registerRender({
    reservationId: id,
    slot: attempt.slot,
    navigation,
    attemptId: attempt.id,
    renderSource,
    winnerContext: { selectedCpm },
  });
}

function claim(
  service: ReturnType<typeof createReservationService>,
  navigation: NavigationSession,
  attempt: RenderAttemptScope,
  id = reservationId(),
  pucSource: object = Object.freeze({})
) {
  return service.claim({
    reservationId: id,
    slot: attempt.slot,
    navigationGeneration: navigation.generation,
    attempt,
    pucSource,
  });
}

function tombstone(
  service: ReturnType<typeof createReservationService>,
  navigation: NavigationSession,
  attempt: RenderAttemptScope,
  id: string,
  state: 'disposed' | 'stale'
) {
  return service.tombstone(
    {
      reservationId: id,
      slot: attempt.slot,
      navigationGeneration: navigation.generation,
      attemptId: attempt.id,
    },
    state
  );
}

describe('renderer reservation identity and registration', () => {
  it.each([
    [reservationId(), true],
    [`r1_${'A'.repeat(22)}`, true],
    [`r1_${'_'.repeat(22)}`, true],
    [`r1_${'-'.repeat(22)}`, true],
    [`r1_${'a'.repeat(21)}`, false],
    [`r1_${'a'.repeat(23)}`, false],
    [`r2_${'a'.repeat(22)}`, false],
    [`r1_${'a'.repeat(21)}=`, false],
    [`r1_${'a'.repeat(21)}+`, false],
    ['', false],
    [undefined, false],
  ])('validates the exact server-minted identity %j', (candidate, expected) => {
    expect(isRendererReservationId(candidate)).toBe(expected);
  });

  it('copies and freezes one exact APS, ADM, or cache source without retaining projection input', () => {
    const { navigation } = runtimeNavigation();
    const sources = [apsSource(), admSource(), cacheSource()];

    for (const [index, source] of sources.entries()) {
      const service = serviceAt(() => 5);
      const attempt = renderAttempt(navigation, `slot-${index}`);
      const mutable = structuredClone(source) as Record<string, unknown>;
      expect(registerRender(service, navigation, attempt, reservationId(index), mutable).ok).toBe(
        true
      );
      mutable.width = 1;

      const result = claim(service, navigation, attempt, reservationId(index));
      expect(result).toMatchObject({ recognized: true, claimed: true });
      if (!result.recognized || !result.claimed) throw new Error('Expected a claim');
      expect(result.renderSource).toEqual(source);
      expect(result.renderSource).not.toBe(mutable);
      expect(Object.isFrozen(result.renderSource)).toBe(true);
      expect(Object.isFrozen(result.winnerContext)).toBe(true);
    }
  });

  it('rejects duplicate identity against live and tombstoned entries without overwriting either', () => {
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => 0);
    const first = renderAttempt(navigation, 'first');
    const second = renderAttempt(navigation, 'second');

    expect(registerRender(service, navigation, first)).toMatchObject({ ok: true });
    expect(registerRender(service, navigation, second)).toEqual({
      ok: false,
      reason: 'reservation_collision',
    });
    expect(claim(service, navigation, first)).toMatchObject({ claimed: true });
    expect(registerRender(service, navigation, second)).toEqual({
      ok: false,
      reason: 'reservation_collision',
    });
  });

  it('rejects nonfinite, negative, accessor, and extra-field winner contexts before publication', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    for (const winnerContext of [
      { selectedCpm: Number.NaN },
      { selectedCpm: Number.POSITIVE_INFINITY },
      { selectedCpm: -0.01 },
      { selectedCpm: 1, extra: true },
      Object.defineProperty({}, 'selectedCpm', { enumerable: true, get: () => 1 }),
    ]) {
      const service = serviceAt(() => 0);
      expect(
        service.registerRender({
          reservationId: reservationId(),
          slot: attempt.slot,
          navigation,
          attemptId: attempt.id,
          renderSource: admSource(),
          winnerContext,
        })
      ).toEqual({ ok: false, reason: 'invalid_winner_context' });
      expect(service.snapshotInventoryForTest().size).toBe(0);
    }
  });

  it('contains hostile sources, owners, and prototype poisoning without partial live publication', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    const hostileSource = Object.defineProperty({}, 'type', {
      enumerable: true,
      get() {
        throw new Error('hostile getter');
      },
    });

    expect(() =>
      registerRender(service, navigation, attempt, reservationId(), hostileSource)
    ).not.toThrow();
    expect(registerRender(service, navigation, attempt, reservationId(), hostileSource)).toEqual({
      ok: false,
      reason: 'invalid_render_source',
    });

    const originalGet = Map.prototype.get;
    const originalSet = Map.prototype.set;
    const originalDelete = Map.prototype.delete;
    Map.prototype.get = function poisonedGet() {
      throw new Error('poisoned get');
    };
    Map.prototype.set = function poisonedSet() {
      throw new Error('poisoned set');
    };
    Map.prototype.delete = function poisonedDelete() {
      throw new Error('poisoned delete');
    };
    try {
      expect(
        service.registerRender({
          reservationId: reservationId(),
          slot: attempt.slot,
          navigation: {
            generation: navigation.generation,
            isCurrent: () => true,
            onDispose: vi.fn(),
          },
          attemptId: attempt.id,
          renderSource: admSource(),
          winnerContext: { selectedCpm: 1.25 },
        })
      ).toMatchObject({ ok: true });
      let adopted: WinnerContext | undefined;
      expect(
        service.claim({
          reservationId: reservationId(),
          slot: attempt.slot,
          navigationGeneration: navigation.generation,
          attempt: {
            id: attempt.id,
            slot: attempt.slot,
            get winnerContext() {
              return adopted;
            },
            isCurrent: () => true,
            adoptWinnerContext: (context) => {
              adopted = context;
              return true;
            },
          },
          pucSource: Object.freeze({}),
        })
      ).toMatchObject({ claimed: true });
    } finally {
      Map.prototype.get = originalGet;
      Map.prototype.set = originalSet;
      Map.prototype.delete = originalDelete;
    }
  });

  it('tombstones a registration if owner generation changes during disposal publication', () => {
    const service = serviceAt(() => 0);
    const initialGeneration = Object.freeze({});
    let generation = initialGeneration;
    const owner: ReservationOwner = {
      get generation() {
        return generation;
      },
      isCurrent: () => true,
      onDispose: () => {
        generation = Object.freeze({});
      },
    };

    expect(
      service.registerRender({
        reservationId: reservationId(),
        slot: 'fictional-slot',
        navigation: owner,
        attemptId: 'a1_0000000000000000000000',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 1 },
      })
    ).toEqual({ ok: false, reason: 'stale_owner' });
    expect(service.recognize(reservationId())).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
  });

  it('makes a late expired owner callback token-safe after the same id is reused', () => {
    let now = 0;
    const service = serviceAt(() => now);
    const generation = Object.freeze({});
    let oldCleanup: (() => void) | undefined;
    const oldOwner: ReservationOwner = {
      generation,
      isCurrent: () => true,
      onDispose: (_kind, callback) => {
        oldCleanup = callback;
      },
    };
    const input = {
      reservationId: reservationId(),
      slot: 'fictional-slot',
      attemptId: 'a1_0000000000000000000000',
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1 },
    };
    expect(service.registerRender({ ...input, navigation: oldOwner })).toMatchObject({ ok: true });
    now = RENDER_RESERVATION_LIFETIME_MS;
    expect(service.recognize(reservationId())).toEqual({ recognized: false });

    const newOwner: ReservationOwner = {
      generation,
      isCurrent: () => true,
      onDispose: vi.fn(),
    };
    expect(service.registerRender({ ...input, navigation: newOwner })).toMatchObject({ ok: true });
    oldCleanup?.();

    expect(service.recognize(reservationId())).toMatchObject({ state: 'renderable' });
  });
});

describe('fixed expiry, capacity, and tombstones', () => {
  it('is live exactly before the 15-minute boundary and prunes at and after expiry', () => {
    for (const offset of [-1, 0, 1]) {
      let now = 100;
      const { navigation } = runtimeNavigation();
      const attempt = renderAttempt(navigation);
      const service = serviceAt(() => now);
      const registration = registerRender(service, navigation, attempt);
      expect(registration).toEqual({ ok: true, expiresAt: 100 + RENDER_RESERVATION_LIFETIME_MS });

      now = 100 + RENDER_RESERVATION_LIFETIME_MS + offset;
      expect(service.recognize(reservationId()).recognized).toBe(offset < 0);
    }
  });

  it('never moves the monotonic clock backward', () => {
    let now = 100;
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => now);
    expect(registerRender(service, navigation, attempt)).toEqual({
      ok: true,
      expiresAt: 100 + RENDER_RESERVATION_LIFETIME_MS,
    });

    now = 1;
    expect(service.recognize(reservationId())).toEqual({
      recognized: true,
      state: 'renderable',
      expiresAt: 100 + RENDER_RESERVATION_LIFETIME_MS,
    });
  });

  it.each([
    ['negative', (): number => -1],
    ['nonfinite', (): number => Number.NaN],
    [
      'throwing',
      (): number => {
        throw new Error('clock failed');
      },
    ],
    ['overflowing deadline', (): number => Number.MAX_VALUE],
  ] as const)('fails closed without publication for a %s monotonic clock', (_name, now) => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(now);

    expect(registerRender(service, navigation, attempt)).toEqual({
      ok: false,
      reason: 'service_disposed',
    });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      disposed: true,
      size: 0,
      live: 0,
      tombstones: 0,
    });
  });

  it('prunes safely while Array push and iteration prototypes are poisoned', () => {
    let now = 0;
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => now);
    registerRender(service, navigation, attempt);
    now = RENDER_RESERVATION_LIFETIME_MS;
    const originalPush = Array.prototype.push;
    const originalIterator = Array.prototype[Symbol.iterator];
    let recognition: ReturnType<typeof service.recognize> | undefined;
    Array.prototype.push = function poisonedPush() {
      throw new Error('poisoned push');
    };
    Array.prototype[Symbol.iterator] = function poisonedIterator() {
      throw new Error('poisoned iterator');
    };
    try {
      recognition = service.recognize(reservationId());
    } finally {
      Array.prototype.push = originalPush;
      Array.prototype[Symbol.iterator] = originalIterator;
    }
    expect(recognition).toEqual({ recognized: false });
  });

  it('uses captured Map iterator operations after their prototypes are poisoned', () => {
    let now = 0;
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => now);
    registerRender(service, navigation, attempt);
    const originalValues = Map.prototype.values;
    const originalEntries = Map.prototype.entries;
    const iteratorPrototype = Object.getPrototypeOf(new Map().values()) as {
      next: () => IteratorResult<unknown>;
    };
    const originalNext = iteratorPrototype.next;
    let recognition: ReturnType<typeof service.recognize> | undefined;
    Map.prototype.values = function poisonedValues() {
      throw new Error('poisoned values');
    };
    Map.prototype.entries = function poisonedEntries() {
      throw new Error('poisoned entries');
    };
    iteratorPrototype.next = function poisonedNext() {
      throw new Error('poisoned next');
    };
    now = RENDER_RESERVATION_LIFETIME_MS;
    try {
      recognition = service.recognize(reservationId());
    } finally {
      Map.prototype.values = originalValues;
      Map.prototype.entries = originalEntries;
      iteratorPrototype.next = originalNext;
    }
    expect(recognition).toEqual({ recognized: false });
  });

  it('consumption never extends expiry and leaves only minimum suppression metadata', () => {
    let now = 200;
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => now);
    registerRender(service, navigation, attempt);
    now = 400;

    expect(claim(service, navigation, attempt)).toMatchObject({ claimed: true });
    expect(service.recognize(reservationId())).toEqual({
      recognized: true,
      state: 'consumed',
      expiresAt: 200 + RENDER_RESERVATION_LIFETIME_MS,
    });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 1,
      entriesWithRenderSource: 0,
      entriesWithWinnerContext: 0,
      entriesWithPucSource: 0,
    });
  });

  it.each(['stale', 'disposed'] as const)(
    'retains an exact %s tombstone through the original expiry',
    (state) => {
      let now = 0;
      const { navigation } = runtimeNavigation();
      const attempt = renderAttempt(navigation);
      const service = serviceAt(() => now);
      registerRender(service, navigation, attempt);
      now = 50;

      expect(tombstone(service, navigation, attempt, reservationId(), state)).toBe(true);
      expect(service.recognize(reservationId())).toEqual({
        recognized: true,
        state,
        expiresAt: RENDER_RESERVATION_LIFETIME_MS,
      });
      expect(service.snapshotInventoryForTest().entriesWithRenderSource).toBe(0);
      now = RENDER_RESERVATION_LIFETIME_MS;
      expect(service.recognize(reservationId())).toEqual({ recognized: false });
    }
  );

  it('allows only the exact slot, generation, and attempt owner to tombstone a live entry', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    registerRender(service, navigation, attempt);
    const exact = {
      reservationId: reservationId(),
      slot: attempt.slot,
      navigationGeneration: navigation.generation,
      attemptId: attempt.id,
    };

    expect(service.tombstone({ ...exact, slot: 'other-slot' }, 'stale')).toBe(false);
    expect(service.tombstone({ ...exact, navigationGeneration: Object.freeze({}) }, 'stale')).toBe(
      false
    );
    expect(service.tombstone({ ...exact, attemptId: `${attempt.id}-other` }, 'stale')).toBe(false);
    expect(service.recognize(reservationId())).toMatchObject({ state: 'renderable' });
    expect(service.tombstone(exact, 'stale')).toBe(true);
  });

  it('shares capacity 320 across live and tombstones, never evicts, and still serves oldest', () => {
    let now = 0;
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => now);
    const attempts: RenderAttemptScope[] = [];
    for (let index = 0; index < 320; index += 1) {
      const attempt = renderAttempt(navigation, `slot-${index}`);
      attempts.push(attempt);
      expect(registerRender(service, navigation, attempt, reservationId(index))).toMatchObject({
        ok: true,
      });
      if (index % 2 === 0) {
        tombstone(service, navigation, attempt, reservationId(index), 'disposed');
      }
    }
    const overflow = renderAttempt(navigation, 'overflow');

    expect(registerRender(service, navigation, overflow, reservationId(320))).toEqual({
      ok: false,
      reason: 'registry_full',
    });
    expect(claim(service, navigation, attempts[1]!, reservationId(1))).toMatchObject({
      claimed: true,
    });
    expect(service.snapshotInventoryForTest().size).toBe(320);

    now = RENDER_RESERVATION_LIFETIME_MS;
    expect(registerRender(service, navigation, overflow, reservationId(320))).toMatchObject({
      ok: true,
    });
  });

  it('automatically tombstones navigation-owned live entries and retains no source/context', () => {
    const { navigation, runtime } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    registerRender(service, navigation, attempt);

    runtime.replaceNavigation();

    expect(service.recognize(reservationId())).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 1,
      entriesWithRenderSource: 0,
      entriesWithWinnerContext: 0,
    });
  });
});

describe('Prebid admission leases and selection', () => {
  it('marks a navigation-disposed Prebid lease aborted through its original short expiry', () => {
    const { navigation, runtime } = runtimeNavigation();
    const service = serviceAt(() => 10);
    service.registerPrebidLease({
      reservationId: reservationId(),
      slot: 'fictional-slot',
      navigation,
      auctionId: 'fictional-auction',
      adUnitCode: 'fictional-slot',
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1 },
      prebidBid: Object.freeze({ cpm: 1 }),
    });

    runtime.replaceNavigation();

    expect(service.recognize(reservationId())).toEqual({
      recognized: true,
      state: 'aborted',
      expiresAt: 10 + PREBID_ADMISSION_LEASE_MS,
    });
  });

  it('does not adopt context when a clock jump makes promotion stale', () => {
    let now = 0;
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => now);
    const bid = Object.freeze({ cpm: 1 });
    service.registerPrebidLease({
      reservationId: reservationId(),
      slot: 'fictional-slot',
      navigation,
      auctionId: 'fictional-auction',
      adUnitCode: 'fictional-slot',
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1 },
      prebidBid: bid,
    });
    const attempt = renderAttempt(navigation);
    now = Number.MAX_VALUE;

    expect(
      service.promotePrebidSelection({
        reservationId: reservationId(),
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        navigationGeneration: navigation.generation,
        attempt,
        prebidBid: bid,
      })
    ).toEqual({ ok: false, reason: 'reservation_not_live' });
    expect(attempt.winnerContext).toBeUndefined();
    expect(service.snapshotInventoryForTest().live).toBe(0);
  });

  it('requires a frozen bid with exact CPM equality and does not retain native Prebid identity', () => {
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => 0);
    const base = {
      reservationId: reservationId(),
      slot: 'fictional-slot',
      navigation,
      auctionId: 'fictional-auction',
      adUnitCode: 'fictional-slot',
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1.25 },
    };

    expect(service.registerPrebidLease({ ...base, prebidBid: { cpm: 1.25 } })).toEqual({
      ok: false,
      reason: 'prebid_cpm_mismatch',
    });
    expect(
      service.registerPrebidLease({ ...base, prebidBid: Object.freeze({ cpm: 2, adId: 'native' }) })
    ).toEqual({ ok: false, reason: 'prebid_cpm_mismatch' });
    expect(
      service.registerPrebidLease({ ...base, prebidBid: Object.freeze({ cpm: 1.25 }) })
    ).toEqual({ ok: true, expiresAt: PREBID_ADMISSION_LEASE_MS });
    expect(service.recognize('native')).toEqual({ recognized: false });
  });

  it('keeps a ten-second suppress-only lease, then atomically promotes the selected id to 15 minutes', () => {
    let now = 10;
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => now);
    const bid = Object.freeze({ cpm: 1.25 });
    const base = {
      slot: 'fictional-slot',
      navigation,
      auctionId: 'fictional-auction',
      adUnitCode: 'fictional-slot',
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1.25 },
      prebidBid: bid,
    };
    expect(service.registerPrebidLease({ ...base, reservationId: reservationId(1) })).toEqual({
      ok: true,
      expiresAt: 10 + PREBID_ADMISSION_LEASE_MS,
    });
    expect(service.registerPrebidLease({ ...base, reservationId: reservationId(2) })).toMatchObject(
      {
        ok: true,
      }
    );
    const attempt = renderAttempt(navigation);
    now = 1_000;

    expect(
      service.promotePrebidSelection({
        reservationId: reservationId(1),
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        navigationGeneration: navigation.generation,
        attempt,
        prebidBid: bid,
      })
    ).toEqual({ ok: true, expiresAt: 1_000 + RENDER_RESERVATION_LIFETIME_MS });
    expect(attempt.winnerContext).toEqual({ selectedCpm: 1.25 });
    expect(service.recognize(reservationId(1))).toMatchObject({
      recognized: true,
      state: 'renderable',
      expiresAt: 1_000 + RENDER_RESERVATION_LIFETIME_MS,
    });
    expect(service.recognize(reservationId(2))).toEqual({
      recognized: true,
      state: 'unselected',
      expiresAt: 10 + PREBID_ADMISSION_LEASE_MS,
    });
  });

  it('promotes only before the admission boundary and prunes at and after ten seconds', () => {
    for (const offset of [-1, 0, 1]) {
      let now = 100;
      const { navigation } = runtimeNavigation();
      const service = serviceAt(() => now);
      const bid = Object.freeze({ cpm: 1 });
      service.registerPrebidLease({
        reservationId: reservationId(),
        slot: 'fictional-slot',
        navigation,
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 1 },
        prebidBid: bid,
      });
      const attempt = renderAttempt(navigation);
      now = 100 + PREBID_ADMISSION_LEASE_MS + offset;

      const result = service.promotePrebidSelection({
        reservationId: reservationId(),
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        navigationGeneration: navigation.generation,
        attempt,
        prebidBid: bid,
      });
      expect(result.ok).toBe(offset < 0);
      expect(service.recognize(reservationId()).recognized).toBe(offset < 0);
    }
  });

  it('tombstones losers only in the selected exact auction and ad unit', () => {
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => 0);
    const bid = Object.freeze({ cpm: 1 });
    const register = (id: string, auctionId: string, adUnitCode: string) =>
      service.registerPrebidLease({
        reservationId: id,
        slot: adUnitCode,
        navigation,
        auctionId,
        adUnitCode,
        renderSource: admSource(),
        winnerContext: { selectedCpm: 1 },
        prebidBid: bid,
      });
    register(reservationId(1), 'selected-auction', 'selected-slot');
    register(reservationId(2), 'selected-auction', 'selected-slot');
    register(reservationId(3), 'other-auction', 'selected-slot');
    register(reservationId(4), 'selected-auction', 'other-slot');
    const attempt = renderAttempt(navigation, 'selected-slot');

    expect(
      service.promotePrebidSelection({
        reservationId: reservationId(1),
        auctionId: 'selected-auction',
        adUnitCode: 'selected-slot',
        navigationGeneration: navigation.generation,
        attempt,
        prebidBid: bid,
      })
    ).toMatchObject({ ok: true });
    expect(service.recognize(reservationId(2))).toMatchObject({ state: 'unselected' });
    expect(service.recognize(reservationId(3))).toMatchObject({
      state: 'awaiting_prebid_selection',
    });
    expect(service.recognize(reservationId(4))).toMatchObject({
      state: 'awaiting_prebid_selection',
    });
  });

  it('does not tombstone a same-string loser owned by another navigation generation', () => {
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => 0);
    const bid = Object.freeze({ cpm: 1 });
    const selected = renderAttempt(navigation, 'fictional-slot');
    service.registerPrebidLease({
      reservationId: reservationId(1),
      slot: 'fictional-slot',
      navigation,
      auctionId: 'reused-auction',
      adUnitCode: 'fictional-slot',
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1 },
      prebidBid: bid,
    });
    const otherGeneration = Object.freeze({});
    service.registerPrebidLease({
      reservationId: reservationId(2),
      slot: 'fictional-slot',
      navigation: {
        generation: otherGeneration,
        isCurrent: () => true,
        onDispose: vi.fn(),
      },
      auctionId: 'reused-auction',
      adUnitCode: 'fictional-slot',
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1 },
      prebidBid: bid,
    });

    expect(
      service.promotePrebidSelection({
        reservationId: reservationId(1),
        auctionId: 'reused-auction',
        adUnitCode: 'fictional-slot',
        navigationGeneration: navigation.generation,
        attempt: selected,
        prebidBid: bid,
      })
    ).toMatchObject({ ok: true });
    expect(service.recognize(reservationId(2))).toMatchObject({
      state: 'awaiting_prebid_selection',
    });
  });

  it('promotes and tombstones losers atomically under poisoned Array prototypes', () => {
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => 0);
    const bid = Object.freeze({ cpm: 1 });
    for (const id of [reservationId(1), reservationId(2)]) {
      service.registerPrebidLease({
        reservationId: id,
        slot: 'fictional-slot',
        navigation,
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 1 },
        prebidBid: bid,
      });
    }
    const attempt = renderAttempt(navigation);
    const originalPush = Array.prototype.push;
    const originalIterator = Array.prototype[Symbol.iterator];
    let result: ReturnType<typeof service.promotePrebidSelection> | undefined;
    Array.prototype.push = function poisonedPush() {
      throw new Error('poisoned push');
    };
    Array.prototype[Symbol.iterator] = function poisonedIterator() {
      throw new Error('poisoned iterator');
    };
    try {
      result = service.promotePrebidSelection({
        reservationId: reservationId(1),
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        navigationGeneration: navigation.generation,
        attempt,
        prebidBid: bid,
      });
    } finally {
      Array.prototype.push = originalPush;
      Array.prototype[Symbol.iterator] = originalIterator;
    }
    expect(result).toMatchObject({ ok: true });
    expect(service.recognize(reservationId(2))).toMatchObject({ state: 'unselected' });
  });

  it.each(['aborted', 'prebid_selection_timeout'] as const)(
    'tombstones %s leases only through their original admission expiry',
    (reason) => {
      let now = 25;
      const { navigation } = runtimeNavigation();
      const service = serviceAt(() => now);
      service.registerPrebidLease({
        reservationId: reservationId(),
        slot: 'fictional-slot',
        navigation,
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 0 },
        prebidBid: Object.freeze({ cpm: 0 }),
      });
      now = 50;

      expect(
        service.tombstonePrebidGroup(
          {
            auctionId: 'fictional-auction',
            adUnitCode: 'fictional-slot',
            navigationGeneration: navigation.generation,
          },
          reason
        )
      ).toBe(1);
      expect(service.recognize(reservationId())).toEqual({
        recognized: true,
        state: reason,
        expiresAt: 25 + PREBID_ADMISSION_LEASE_MS,
      });
    }
  );

  it('makes a stale navigation Prebid group tombstone callback inert', () => {
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => 0);
    service.registerPrebidLease({
      reservationId: reservationId(),
      slot: 'fictional-slot',
      navigation,
      auctionId: 'reused-auction',
      adUnitCode: 'fictional-slot',
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1 },
      prebidBid: Object.freeze({ cpm: 1 }),
    });

    expect(
      service.tombstonePrebidGroup(
        {
          auctionId: 'reused-auction',
          adUnitCode: 'fictional-slot',
          navigationGeneration: Object.freeze({}),
        },
        'aborted'
      )
    ).toBe(0);
    expect(service.recognize(reservationId())).toMatchObject({
      state: 'awaiting_prebid_selection',
    });
    expect(
      service.tombstonePrebidGroup(
        {
          auctionId: 'reused-auction',
          adUnitCode: 'fictional-slot',
          navigationGeneration: navigation.generation,
        },
        'aborted'
      )
    ).toBe(1);
  });

  it('suppresses and contract-failure tombstones a PUC claim against a preselection lease', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    service.registerPrebidLease({
      reservationId: reservationId(),
      slot: attempt.slot,
      navigation,
      auctionId: 'fictional-auction',
      adUnitCode: attempt.slot,
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1 },
      prebidBid: Object.freeze({ cpm: 1 }),
    });

    expect(claim(service, navigation, attempt)).toEqual({
      recognized: true,
      claimed: false,
      state: 'prebid_contract_violation',
    });
    expect(service.recognize(reservationId())).toEqual({
      recognized: true,
      state: 'prebid_contract_violation',
      expiresAt: PREBID_ADMISSION_LEASE_MS,
    });
    expect(attempt.winnerContext).toBeUndefined();
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 1,
      entriesWithRenderSource: 0,
      entriesWithWinnerContext: 0,
    });
  });

  it.each(['prebid_admission_failed', 'prebid_contract_violation'] as const)(
    'tombstones exact-owner %s admission failure through the original lease',
    (state) => {
      const { navigation } = runtimeNavigation();
      const service = serviceAt(() => 0);
      service.registerPrebidLease({
        reservationId: reservationId(),
        slot: 'fictional-slot',
        navigation,
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 1 },
        prebidBid: Object.freeze({ cpm: 1 }),
      });
      const exact = {
        reservationId: reservationId(),
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        navigationGeneration: navigation.generation,
      };

      expect(
        service.tombstonePrebidLease({ ...exact, navigationGeneration: Object.freeze({}) }, state)
      ).toBe(false);
      expect(service.tombstonePrebidLease(exact, state)).toBe(true);
      expect(service.recognize(reservationId())).toEqual({
        recognized: true,
        state,
        expiresAt: PREBID_ADMISSION_LEASE_MS,
      });
    }
  );
});

describe('atomic claims and disposal', () => {
  it('does not acquire, transfer, or consume for a mismatched slot, generation, attempt, or stale owner', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    const source = Object.freeze({});
    const cases = [
      { slot: 'other-slot', generation: navigation.generation, attempted: attempt },
      { slot: attempt.slot, generation: Object.freeze({}), attempted: attempt },
      {
        slot: attempt.slot,
        generation: navigation.generation,
        attempted: { ...attempt, id: `${attempt.id}-other` },
      },
      {
        slot: attempt.slot,
        generation: navigation.generation,
        attempted: { ...attempt, isCurrent: () => false },
      },
    ];

    for (const [index, candidate] of cases.entries()) {
      const id = reservationId(index + 10);
      registerRender(service, navigation, attempt, id);
      expect(
        service.claim({
          reservationId: id,
          slot: candidate.slot,
          navigationGeneration: candidate.generation,
          attempt: candidate.attempted,
          pucSource: source,
        })
      ).toEqual({ recognized: true, claimed: false, state: 'renderable' });
      expect(service.recognize(id)).toMatchObject({ state: 'renderable' });
    }
    expect(attempt.winnerContext).toBeUndefined();
    expect(service.snapshotInventoryForTest().entriesWithPucSource).toBe(0);
  });
  it('transfers immutable context before consumption and preserves it after projection replacement', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    const source = admSource('<div>original winner</div>');
    const context = { selectedCpm: 7.5 };
    service.registerRender({
      reservationId: reservationId(),
      slot: attempt.slot,
      navigation,
      attemptId: attempt.id,
      renderSource: source,
      winnerContext: context,
    });
    context.selectedCpm = 99;
    const observedStates: string[] = [];
    const sink = {
      id: attempt.id,
      slot: attempt.slot,
      get winnerContext(): WinnerContext | undefined {
        return attempt.winnerContext;
      },
      isCurrent: () => attempt.isCurrent(),
      adoptWinnerContext(winnerContext: WinnerContext): boolean {
        const recognition = service.recognize(reservationId());
        if (recognition.recognized) observedStates.push(recognition.state);
        return attempt.adoptWinnerContext(winnerContext);
      },
    };

    const result = service.claim({
      reservationId: reservationId(),
      slot: attempt.slot,
      navigationGeneration: navigation.generation,
      attempt: sink,
      pucSource: Object.freeze({}),
    });

    expect(observedStates).toEqual(['renderable']);
    expect(result).toMatchObject({ recognized: true, claimed: true });
    expect(attempt.winnerContext).toEqual({ selectedCpm: 7.5 });
    expect(Object.isFrozen(attempt.winnerContext)).toBe(true);
    expect(service.recognize(reservationId())).toMatchObject({ state: 'consumed' });
  });

  it('allows exactly one of two simultaneous/reentrant claims and never replaces its PUC source', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    registerRender(service, navigation, attempt);
    const firstSource = Object.freeze({ name: 'first' });
    const secondSource = Object.freeze({ name: 'second' });
    let nested: ReturnType<typeof service.claim> | undefined;
    let acceptedContext: WinnerContext | undefined;
    const sink = {
      id: attempt.id,
      slot: attempt.slot,
      get winnerContext(): WinnerContext | undefined {
        return acceptedContext;
      },
      isCurrent: () => true,
      adoptWinnerContext(context: WinnerContext): boolean {
        nested = service.claim({
          reservationId: reservationId(),
          slot: attempt.slot,
          navigationGeneration: navigation.generation,
          attempt: sink,
          pucSource: secondSource,
        });
        acceptedContext = context;
        return true;
      },
    };

    const first = service.claim({
      reservationId: reservationId(),
      slot: attempt.slot,
      navigationGeneration: navigation.generation,
      attempt: sink,
      pucSource: firstSource,
    });

    expect(first).toMatchObject({ recognized: true, claimed: true, pucSource: firstSource });
    expect(nested).toEqual({ recognized: true, claimed: false, state: 'renderable' });
    expect(claim(service, navigation, attempt, reservationId(), secondSource)).toEqual({
      recognized: true,
      claimed: false,
      state: 'consumed',
    });
  });

  it('rolls back a throwing context transfer without retaining the attempted PUC source', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    registerRender(service, navigation, attempt);
    const throwingSink = {
      id: attempt.id,
      slot: attempt.slot,
      winnerContext: undefined,
      isCurrent: () => true,
      adoptWinnerContext(): boolean {
        throw new Error('partial transfer failed');
      },
    };

    expect(
      service.claim({
        reservationId: reservationId(),
        slot: attempt.slot,
        navigationGeneration: navigation.generation,
        attempt: throwingSink,
        pucSource: Object.freeze({}),
      })
    ).toEqual({ recognized: true, claimed: false, state: 'renderable' });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 1,
      entriesWithPucSource: 0,
    });
  });

  it('retains only a disposed suppression tombstone when owner publication rolls back', () => {
    const service = serviceAt(() => 0);
    const generation = Object.freeze({});
    const owner: ReservationOwner = {
      generation,
      isCurrent: () => true,
      onDispose: (_kind, callback) => {
        callback();
        throw new Error('publication failed after disposal');
      },
    };

    expect(
      service.registerRender({
        reservationId: reservationId(),
        slot: 'fictional-slot',
        navigation: owner,
        attemptId: 'a1_0000000000000000000000',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 1 },
      })
    ).toEqual({ ok: false, reason: 'stale_owner' });
    expect(service.recognize(reservationId())).toMatchObject({
      recognized: true,
      state: 'disposed',
    });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 1,
      entriesWithRenderSource: 0,
      entriesWithWinnerContext: 0,
    });
  });

  it('disposes the whole runtime store without making old identities reusable in that service', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    registerRender(service, navigation, attempt);

    service.dispose();

    expect(service.snapshotInventoryForTest()).toMatchObject({ disposed: true, size: 0 });
    expect(registerRender(service, navigation, attempt)).toEqual({
      ok: false,
      reason: 'service_disposed',
    });
    expect(service.recognize(reservationId())).toEqual({ recognized: false });
  });
});
