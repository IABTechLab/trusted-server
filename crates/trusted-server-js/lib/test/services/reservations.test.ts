import { describe, expect, it, vi } from 'vitest';

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
  type ReservationRenderSource,
} from '../../src/services/reservations';

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
  return createReservationService({
    now: readNow,
    prepareRenderSource: (candidate) => {
      const source = parseBidRenderSourceV1(candidate);
      return source?.type === 'pbs_cache' ? undefined : source;
    },
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
  it('atomically adopts unexpired first-display tombstones in the local clock epoch', () => {
    let now = 100;
    const service = serviceAt(() => now);

    expect(
      service.adoptFirstDisplayTombstones({
        clockEpochMs: 40,
        tombstones: [
          { expiresAtMs: 140, reservationId: reservationId(1) },
          { expiresAtMs: 30, reservationId: reservationId(2) },
        ],
      })
    ).toBe(true);
    expect(service.recognize(reservationId(1))).toMatchObject({
      recognized: true,
      state: 'consumed',
      expiresAt: 200,
    });
    expect(service.recognize(reservationId(2))).toEqual({ recognized: false });

    now = 200;
    expect(service.recognize(reservationId(1))).toEqual({ recognized: false });
    expect(service.adoptFirstDisplayTombstones({ clockEpochMs: 200, tombstones: [] })).toBe(false);
  });

  it('rejects malformed first-display tombstones without publishing a partial set', () => {
    const service = serviceAt(() => 100);

    expect(
      service.adoptFirstDisplayTombstones({
        clockEpochMs: 50,
        tombstones: [
          { expiresAtMs: 200, reservationId: reservationId(1) },
          { expiresAtMs: 210, reservationId: reservationId(1) },
        ],
      })
    ).toBe(false);
    expect(service.recognize(reservationId(1))).toEqual({ recognized: false });
  });

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

  it('copies and freezes one exact APS or ADM source without retaining projection input', () => {
    const { navigation } = runtimeNavigation();
    const sources = [apsSource(), admSource()];

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
      const context = attempt.winnerContext;
      if (!context) throw new Error('Expected an admitted winner context');
      const admission = service.consumeClaim(result, {
        attempt,
        attemptId: attempt.id,
        slot: attempt.slot,
        navigationGeneration: navigation.generation,
        winnerContext: context,
      });
      expect(admission?.renderSource).toEqual(source);
      expect(admission?.renderSource).not.toBe(mutable);
      expect(Object.isFrozen(admission?.renderSource)).toBe(true);
      expect(Object.isFrozen(admission?.winnerContext)).toBe(true);
    }
  });

  it('binds one consumed claim object to its exact attempt source and winner context', () => {
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => 5);
    const attempt = renderAttempt(navigation);
    expect(registerRender(service, navigation, attempt)).toMatchObject({ ok: true });
    const result = claim(service, navigation, attempt);
    if (!result.recognized || !result.claimed) throw new Error('Expected a claim');
    expect(Object.getOwnPropertyNames(result).sort()).toEqual([
      'claimed',
      'expiresAt',
      'pucSource',
      'recognized',
    ]);
    expect(result).not.toHaveProperty('renderSource');
    expect(result).not.toHaveProperty('winnerContext');
    const context = attempt.winnerContext;
    if (!context) throw new Error('Expected an admitted winner context');

    expect(
      Reflect.apply(service.consumeClaim, service, [
        result,
        {
          attemptId: attempt.id,
          slot: attempt.slot,
          navigationGeneration: navigation.generation,
          winnerContext: context,
        },
      ])
    ).toBeUndefined();
    const replayedAttempt = Object.freeze({ ...attempt });
    expect(
      service.consumeClaim(result, {
        attempt: replayedAttempt,
        attemptId: attempt.id,
        slot: attempt.slot,
        navigationGeneration: navigation.generation,
        winnerContext: context,
      })
    ).toBeUndefined();
    expect(
      service.consumeClaim(result, {
        attempt,
        attemptId: attempt.id,
        slot: attempt.slot,
        navigationGeneration: Object.freeze({}),
        winnerContext: context,
      })
    ).toBeUndefined();
    expect(
      service.consumeClaim(result, {
        attempt,
        attemptId: attempt.id,
        slot: attempt.slot,
        navigationGeneration: navigation.generation,
        winnerContext: Object.freeze({ selectedCpm: context.selectedCpm }),
      })
    ).toBeUndefined();
    expect(
      service.consumeClaim(Object.freeze({ ...result }), {
        attempt,
        attemptId: attempt.id,
        slot: attempt.slot,
        navigationGeneration: navigation.generation,
        winnerContext: context,
      })
    ).toBeUndefined();

    const admission = service.consumeClaim(result, {
      attempt,
      attemptId: attempt.id,
      slot: attempt.slot,
      navigationGeneration: navigation.generation,
      winnerContext: context,
    });
    expect(admission).toEqual({
      renderSource: admSource(),
      winnerContext: context,
    });
    expect(admission?.winnerContext).toBe(context);
    expect(Object.isFrozen(admission)).toBe(true);
    expect(
      service.consumeClaim(result, {
        attempt,
        attemptId: attempt.id,
        slot: attempt.slot,
        navigationGeneration: navigation.generation,
        winnerContext: context,
      })
    ).toBeUndefined();
  });

  it.each(['navigation_disposed', 'service_disposed', 'expired'] as const)(
    'invalidates a consumed claim when its authority is %s',
    (mode) => {
      let now = 5;
      const { navigation } = runtimeNavigation();
      const service = serviceAt(() => now);
      const attempt = renderAttempt(navigation);
      expect(registerRender(service, navigation, attempt)).toMatchObject({ ok: true });
      const result = claim(service, navigation, attempt);
      if (!result.recognized || !result.claimed) throw new Error('Expected a claim');
      const context = attempt.winnerContext;
      if (!context) throw new Error('Expected an admitted winner context');

      if (mode === 'navigation_disposed') navigation.dispose();
      else if (mode === 'service_disposed') service.dispose();
      else now = result.expiresAt;

      expect(
        service.consumeClaim(result, {
          attempt,
          attemptId: attempt.id,
          slot: attempt.slot,
          navigationGeneration: navigation.generation,
          winnerContext: context,
        })
      ).toBeUndefined();
    }
  );

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
            prepareWinnerContext: (context) => {
              return {
                commit: () => {
                  adopted = context;
                  return true;
                },
                rollback: () => {
                  if (adopted === context) adopted = undefined;
                  return true;
                },
              };
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

  it('uses captured identity and UTF-8 validators after their prototypes are poisoned', () => {
    const generation = Object.freeze({});
    const renderSource = admSource() as ReservationRenderSource;
    const service = createReservationService({
      now: () => 0,
      prepareRenderSource: (candidate) => (candidate === renderSource ? renderSource : undefined),
    });
    const owner: ReservationOwner = {
      generation,
      isCurrent: () => true,
      onDispose: () => undefined,
    };
    const originalRegExpTest = RegExp.prototype.test;
    const originalTextEncoderEncode = TextEncoder.prototype.encode;
    let validIdentity: boolean | undefined;
    let invalidIdentity: boolean | undefined;
    let invalidSlot: ReturnType<typeof service.registerRender> | undefined;
    let validRegistration: ReturnType<typeof service.registerRender> | undefined;
    let thrown: unknown;

    RegExp.prototype.test = function poisonedRegExpTest() {
      throw new Error('poisoned RegExp.test');
    };
    TextEncoder.prototype.encode = function poisonedTextEncoderEncode() {
      throw new Error('poisoned TextEncoder.encode');
    };
    try {
      validIdentity = isRendererReservationId(reservationId());
      invalidIdentity = isRendererReservationId('not-a-reservation');
      invalidSlot = service.registerRender({
        reservationId: reservationId(),
        slot: 'x'.repeat(257),
        navigation: owner,
        attemptId: 'a1_0000000000000000000000',
        renderSource,
        winnerContext: { selectedCpm: 1 },
      });
      validRegistration = service.registerRender({
        reservationId: reservationId(),
        slot: 'fictional-slot',
        navigation: owner,
        attemptId: 'a1_0000000000000000000000',
        renderSource,
        winnerContext: { selectedCpm: 1 },
      });
    } catch (error) {
      thrown = error;
    } finally {
      RegExp.prototype.test = originalRegExpTest;
      TextEncoder.prototype.encode = originalTextEncoderEncode;
    }

    expect(thrown).toBeUndefined();
    expect(validIdentity).toBe(true);
    expect(invalidIdentity).toBe(false);
    expect(invalidSlot).toEqual({ ok: false, reason: 'invalid_slot' });
    expect(validRegistration).toMatchObject({ ok: true });
  });

  it('uses captured code-unit validation when String.charCodeAt returns benign data', () => {
    const generation = Object.freeze({});
    const renderSource = admSource() as ReservationRenderSource;
    const service = createReservationService({
      now: () => 0,
      prepareRenderSource: (candidate) => (candidate === renderSource ? renderSource : undefined),
    });
    const originalCharCodeAt = String.prototype.charCodeAt;
    const results: ReturnType<typeof service.registerRender>[] = [];
    String.prototype.charCodeAt = () => 0x61;
    try {
      for (const [index, slot] of ['control\u0000slot', 'lone-surrogate\ud800'].entries()) {
        results[results.length] = service.registerRender({
          reservationId: reservationId(index),
          slot,
          navigation: { generation, isCurrent: () => true, onDispose: () => undefined },
          attemptId: 'a1_0000000000000000000000',
          renderSource,
          winnerContext: { selectedCpm: 1 },
        });
      }
    } finally {
      String.prototype.charCodeAt = originalCharCodeAt;
    }

    expect(results).toEqual([
      { ok: false, reason: 'invalid_slot' },
      { ok: false, reason: 'invalid_slot' },
    ]);
    expect(service.snapshotInventoryForTest().size).toBe(0);
  });

  it('contains throwing String.charCodeAt poisoning without publishing', () => {
    const generation = Object.freeze({});
    const renderSource = admSource() as ReservationRenderSource;
    const service = createReservationService({
      now: () => 0,
      prepareRenderSource: (candidate) => (candidate === renderSource ? renderSource : undefined),
    });
    const originalCharCodeAt = String.prototype.charCodeAt;
    let result: ReturnType<typeof service.registerRender> | undefined;
    let thrown: unknown;
    String.prototype.charCodeAt = () => {
      throw new Error('poisoned String.charCodeAt');
    };
    try {
      result = service.registerRender({
        reservationId: reservationId(),
        slot: 'control\u0000slot',
        navigation: { generation, isCurrent: () => true, onDispose: () => undefined },
        attemptId: 'a1_0000000000000000000000',
        renderSource,
        winnerContext: { selectedCpm: 1 },
      });
    } catch (error) {
      thrown = error;
    } finally {
      String.prototype.charCodeAt = originalCharCodeAt;
    }

    expect(thrown).toBeUndefined();
    expect(result).toEqual({ ok: false, reason: 'invalid_slot' });
    expect(service.snapshotInventoryForTest().size).toBe(0);
  });

  it.each([
    ['throws before applying', false],
    ['throws after applying', true],
  ] as const)('contains a captured Map.set that %s', async (_name, applyFirst) => {
    vi.resetModules();
    const originalSet = Map.prototype.set;
    Map.prototype.set = function poisonedReservationSet(key, value) {
      if (typeof key !== 'string' || !key.startsWith('r1_')) {
        return Reflect.apply(originalSet, this, [key, value]) as Map<unknown, unknown>;
      }
      if (applyFirst) Reflect.apply(originalSet, this, [key, value]);
      throw new Error('captured reservation Map.set failure');
    };
    let isolated: typeof import('../../src/services/reservations');
    try {
      isolated = await import('../../src/services/reservations');
    } finally {
      Map.prototype.set = originalSet;
    }
    const generation = Object.freeze({});
    let cleanup: (() => void) | undefined;
    const renderSource = admSource() as ReservationRenderSource;
    const service = isolated.createReservationService({
      now: () => 0,
      prepareRenderSource: (candidate) => (candidate === renderSource ? renderSource : undefined),
    });
    let result: ReturnType<typeof service.registerRender> | undefined;
    let thrown: unknown;
    try {
      result = service.registerRender({
        reservationId: reservationId(),
        slot: 'fictional-slot',
        navigation: {
          generation,
          isCurrent: () => true,
          onDispose: (_kind, callback) => {
            cleanup = callback;
          },
        },
        attemptId: 'a1_0000000000000000000000',
        renderSource,
        winnerContext: { selectedCpm: 1 },
      });
    } catch (error) {
      thrown = error;
    }

    expect(thrown).toBeUndefined();
    if (applyFirst) {
      expect(result).toMatchObject({ ok: true });
      expect(service.recognize(reservationId())).toMatchObject({ state: 'renderable' });
      cleanup?.();
      expect(service.recognize(reservationId())).toMatchObject({ state: 'disposed' });
    } else {
      expect(result).toEqual({ ok: false, reason: 'service_disposed' });
      expect(service.recognize(reservationId())).toEqual({ recognized: false });
      expect(cleanup).toBeTypeOf('function');
      expect(() => cleanup?.()).not.toThrow();
    }
  });

  it.each([
    ['throws before applying', false],
    ['throws after applying', true],
  ] as const)('contains a captured WeakMap.set that %s', async (_name, applyFirst) => {
    vi.resetModules();
    const originalSet = WeakMap.prototype.set;
    WeakMap.prototype.set = function poisonedOwnerSet(key, value) {
      const record = value as Record<string, unknown> | undefined;
      if (!record || !('identity' in record) || !('ready' in record)) {
        return Reflect.apply(originalSet, this, [key, value]) as WeakMap<object, unknown>;
      }
      if (applyFirst) Reflect.apply(originalSet, this, [key, value]);
      throw new Error('captured owner WeakMap.set failure');
    };
    let isolated: typeof import('../../src/services/reservations');
    try {
      isolated = await import('../../src/services/reservations');
    } finally {
      WeakMap.prototype.set = originalSet;
    }
    const generation = Object.freeze({});
    let cleanup: (() => void) | undefined;
    const renderSource = admSource() as ReservationRenderSource;
    const service = isolated.createReservationService({
      now: () => 0,
      prepareRenderSource: (candidate) => (candidate === renderSource ? renderSource : undefined),
    });
    let result: ReturnType<typeof service.registerRender> | undefined;
    let thrown: unknown;
    try {
      result = service.registerRender({
        reservationId: reservationId(),
        slot: 'fictional-slot',
        navigation: {
          generation,
          isCurrent: () => true,
          onDispose: (_kind, callback) => {
            cleanup = callback;
          },
        },
        attemptId: 'a1_0000000000000000000000',
        renderSource,
        winnerContext: { selectedCpm: 1 },
      });
    } catch (error) {
      thrown = error;
    }

    expect(thrown).toBeUndefined();
    if (applyFirst) {
      expect(result).toMatchObject({ ok: true });
      expect(service.recognize(reservationId())).toMatchObject({ state: 'renderable' });
      cleanup?.();
      expect(service.recognize(reservationId())).toMatchObject({ state: 'disposed' });
    } else {
      expect(result).toEqual({ ok: false, reason: 'stale_owner' });
      expect(cleanup).toBeUndefined();
      expect(service.recognize(reservationId())).toMatchObject({ state: 'disposed' });
    }
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 1,
      entriesWithRenderSource: 0,
      entriesWithWinnerContext: 0,
    });
  });

  it('contains a captured WeakMap.get failure in a navigation callback', async () => {
    vi.resetModules();
    const originalGet = WeakMap.prototype.get;
    let poisoned = false;
    WeakMap.prototype.get = function poisonedOwnerGet(key) {
      if (poisoned) throw new Error('captured owner WeakMap.get failure');
      return Reflect.apply(originalGet, this, [key]) as unknown;
    };
    let isolated: typeof import('../../src/services/reservations');
    try {
      isolated = await import('../../src/services/reservations');
    } finally {
      WeakMap.prototype.get = originalGet;
    }
    const generation = Object.freeze({});
    let cleanup: (() => void) | undefined;
    const renderSource = admSource() as ReservationRenderSource;
    const service = isolated.createReservationService({
      now: () => 0,
      prepareRenderSource: (candidate) => (candidate === renderSource ? renderSource : undefined),
    });
    expect(
      service.registerRender({
        reservationId: reservationId(),
        slot: 'fictional-slot',
        navigation: {
          generation,
          isCurrent: () => true,
          onDispose: (_kind, callback) => {
            cleanup = callback;
          },
        },
        attemptId: 'a1_0000000000000000000000',
        renderSource,
        winnerContext: { selectedCpm: 1 },
      })
    ).toMatchObject({ ok: true });
    poisoned = true;

    expect(() => cleanup?.()).not.toThrow();
    expect(service.recognize(reservationId())).toMatchObject({ state: 'disposed' });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      entriesWithRenderSource: 0,
      entriesWithWinnerContext: 0,
    });
  });

  it('checks publication identity after the final reentrant owner call', () => {
    const service = serviceAt(() => 0);
    const generation = Object.freeze({});
    let cleanup: (() => void) | undefined;
    let currentChecks = 0;
    const owner: ReservationOwner = {
      generation,
      isCurrent: () => {
        currentChecks += 1;
        if (currentChecks === 3) cleanup?.();
        return true;
      },
      onDispose: (_kind, callback) => {
        cleanup = callback;
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
    expect(service.recognize(reservationId())).toMatchObject({ state: 'disposed' });
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

  it('preserves the established callback when another identity reuses its live generation', () => {
    const service = serviceAt(() => 0);
    const generation = Object.freeze({});
    let establishedCleanup: (() => void) | undefined;
    const firstOwner: ReservationOwner = {
      generation,
      isCurrent: () => true,
      onDispose: (_kind, callback) => {
        establishedCleanup = callback;
      },
    };
    expect(
      service.registerRender({
        reservationId: reservationId(),
        slot: 'first-slot',
        navigation: firstOwner,
        attemptId: 'a1_0000000000000000000000',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 1 },
      })
    ).toMatchObject({ ok: true });
    const replacementOnDispose = vi.fn(() => {
      throw new Error('replacement callback publication failed');
    });

    expect(
      service.registerRender({
        reservationId: reservationId(1),
        slot: 'second-slot',
        navigation: {
          generation,
          isCurrent: () => true,
          onDispose: replacementOnDispose,
        },
        attemptId: 'a1_0000000000000000000001',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 2 },
      })
    ).toEqual({ ok: false, reason: 'stale_owner' });
    expect(replacementOnDispose).not.toHaveBeenCalled();
    expect(service.recognize(reservationId())).toMatchObject({ state: 'renderable' });
    expect(service.recognize(reservationId(1))).toMatchObject({ state: 'disposed' });

    establishedCleanup?.();

    expect(service.recognize(reservationId())).toMatchObject({ state: 'disposed' });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 2,
      entriesWithRenderSource: 0,
      entriesWithWinnerContext: 0,
    });
  });

  it('requires a fresh generation when a different owner identity arrives after expiry', () => {
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
    expect(service.registerRender({ ...input, navigation: newOwner })).toEqual({
      ok: false,
      reason: 'stale_owner',
    });
    expect(newOwner.onDispose).not.toHaveBeenCalled();
    oldCleanup?.();

    expect(service.recognize(reservationId())).toMatchObject({ state: 'disposed' });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 1,
      entriesWithRenderSource: 0,
      entriesWithWinnerContext: 0,
    });
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

  it.each([
    [
      'throwing',
      (): number => {
        throw new Error('clock failed');
      },
    ],
    ['nonfinite', (): number => Number.NaN],
    ['backward', (): number => 99],
  ] as const)('retains and suppresses every known id after a %s clock fault', (_name, fault) => {
    let readNow = (): number => 100;
    const { navigation } = runtimeNavigation();
    const liveAttempt = renderAttempt(navigation, 'live-slot');
    const tombstonedAttempt = renderAttempt(navigation, 'tombstoned-slot');
    const nextAttempt = renderAttempt(navigation, 'next-slot');
    const service = serviceAt(() => readNow());
    expect(registerRender(service, navigation, liveAttempt, reservationId())).toMatchObject({
      ok: true,
    });
    expect(registerRender(service, navigation, tombstonedAttempt, reservationId(1))).toMatchObject({
      ok: true,
    });
    expect(tombstone(service, navigation, tombstonedAttempt, reservationId(1), 'disposed')).toBe(
      true
    );

    readNow = fault;

    expect(service.recognize(reservationId())).toMatchObject({ state: 'renderable' });
    expect(service.recognize(reservationId(1))).toMatchObject({ state: 'disposed' });
    expect(claim(service, navigation, liveAttempt)).toEqual({
      recognized: true,
      claimed: false,
      state: 'renderable',
    });
    expect(claim(service, navigation, tombstonedAttempt, reservationId(1))).toEqual({
      recognized: true,
      claimed: false,
      state: 'disposed',
    });
    expect(registerRender(service, navigation, nextAttempt, reservationId(2))).toEqual({
      ok: false,
      reason: 'service_disposed',
    });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      clockFaulted: true,
      disposed: false,
      size: 2,
      live: 1,
      tombstones: 1,
    });
  });

  it('releases live render and lease payloads when navigation disposes after a clock fault', () => {
    let now = 100;
    const { navigation, runtime } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => now);
    expect(registerRender(service, navigation, attempt, reservationId())).toEqual({
      ok: true,
      expiresAt: 100 + RENDER_RESERVATION_LIFETIME_MS,
    });
    expect(
      service.registerPrebidLease({
        reservationId: reservationId(1),
        slot: 'fictional-slot',
        navigation,
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 1 },
        prebidBid: Object.freeze({ cpm: 1 }),
      })
    ).toEqual({ ok: true, expiresAt: 100 + PREBID_ADMISSION_LEASE_MS });
    now = Number.NaN;
    expect(service.recognize(reservationId())).toMatchObject({ state: 'renderable' });

    runtime.replaceNavigation();

    expect(service.recognize(reservationId())).toEqual({
      recognized: true,
      state: 'disposed',
      expiresAt: 100 + RENDER_RESERVATION_LIFETIME_MS,
    });
    expect(service.recognize(reservationId(1))).toEqual({
      recognized: true,
      state: 'aborted',
      expiresAt: 100 + PREBID_ADMISSION_LEASE_MS,
    });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      clockFaulted: true,
      live: 0,
      tombstones: 2,
      entriesWithRenderSource: 0,
      entriesWithWinnerContext: 0,
      entriesWithPucSource: 0,
    });
  });

  it('allows exact explicit terminal tombstones after a clock fault', () => {
    let now = 100;
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => now);
    registerRender(service, navigation, attempt, reservationId());
    const bid = Object.freeze({ cpm: 1 });
    const registerLease = (id: string, auctionId: string) =>
      service.registerPrebidLease({
        reservationId: id,
        slot: 'fictional-slot',
        navigation,
        auctionId,
        adUnitCode: 'fictional-slot',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 1 },
        prebidBid: bid,
      });
    registerLease(reservationId(1), 'single-auction');
    registerLease(reservationId(2), 'group-auction');
    registerLease(reservationId(3), 'group-auction');
    now = Number.NaN;
    expect(service.recognize(reservationId())).toMatchObject({ state: 'renderable' });

    expect(tombstone(service, navigation, attempt, reservationId(), 'stale')).toBe(true);
    expect(
      service.tombstonePrebidLease(
        {
          reservationId: reservationId(1),
          auctionId: 'single-auction',
          adUnitCode: 'fictional-slot',
          navigationGeneration: navigation.generation,
        },
        'prebid_admission_failed'
      )
    ).toBe(true);
    expect(
      service.tombstonePrebidGroup(
        {
          auctionId: 'group-auction',
          adUnitCode: 'fictional-slot',
          navigationGeneration: navigation.generation,
        },
        'prebid_selection_timeout'
      )
    ).toBe(2);

    expect(service.recognize(reservationId())).toEqual({
      recognized: true,
      state: 'stale',
      expiresAt: 100 + RENDER_RESERVATION_LIFETIME_MS,
    });
    for (const [index, state] of [
      [1, 'prebid_admission_failed'],
      [2, 'prebid_selection_timeout'],
      [3, 'prebid_selection_timeout'],
    ] as const) {
      expect(service.recognize(reservationId(index))).toEqual({
        recognized: true,
        state,
        expiresAt: 100 + PREBID_ADMISSION_LEASE_MS,
      });
    }
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 4,
      entriesWithRenderSource: 0,
      entriesWithWinnerContext: 0,
      entriesWithPucSource: 0,
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

  it('rejects invalid runtime tombstone states without changing live entries', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    const bid = Object.freeze({ cpm: 1 });
    registerRender(service, navigation, attempt, reservationId());
    for (const index of [1, 2]) {
      service.registerPrebidLease({
        reservationId: reservationId(index),
        slot: 'fictional-slot',
        navigation,
        auctionId: 'fictional-auction',
        adUnitCode: 'fictional-slot',
        renderSource: admSource(),
        winnerContext: { selectedCpm: 1 },
        prebidBid: bid,
      });
    }
    const hostileState = Object.defineProperty({}, Symbol.toPrimitive, {
      value() {
        throw new Error('state must not be coerced');
      },
    });

    expect(
      service.tombstone(
        {
          reservationId: reservationId(),
          slot: attempt.slot,
          navigationGeneration: navigation.generation,
          attemptId: attempt.id,
        },
        hostileState as never
      )
    ).toBe(false);
    expect(
      service.tombstonePrebidLease(
        {
          reservationId: reservationId(1),
          auctionId: 'fictional-auction',
          adUnitCode: 'fictional-slot',
          navigationGeneration: navigation.generation,
        },
        'consumed' as never
      )
    ).toBe(false);
    expect(
      service.tombstonePrebidGroup(
        {
          auctionId: 'fictional-auction',
          adUnitCode: 'fictional-slot',
          navigationGeneration: navigation.generation,
        },
        'renderable' as never
      )
    ).toBe(0);
    expect(service.recognize(reservationId())).toMatchObject({ state: 'renderable' });
    expect(service.recognize(reservationId(1))).toMatchObject({
      state: 'awaiting_prebid_selection',
    });
    expect(service.recognize(reservationId(2))).toMatchObject({
      state: 'awaiting_prebid_selection',
    });
    expect(service.snapshotInventoryForTest()).toMatchObject({ live: 3, tombstones: 0 });
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

  it('installs one owner disposer across sequential expired leases', () => {
    let now = 0;
    const { navigation } = runtimeNavigation();
    const service = serviceAt(() => now);
    const bid = Object.freeze({ cpm: 1 });

    for (let index = 0; index < 1_000; index += 1) {
      expect(
        service.registerPrebidLease({
          reservationId: reservationId(),
          slot: 'fictional-slot',
          navigation,
          auctionId: `auction-${index}`,
          adUnitCode: 'fictional-slot',
          renderSource: admSource(),
          winnerContext: { selectedCpm: 1 },
          prebidBid: bid,
        })
      ).toMatchObject({ ok: true });
      now += PREBID_ADMISSION_LEASE_MS;
      expect(service.recognize(reservationId())).toEqual({ recognized: false });
    }

    expect(navigation.snapshotInventoryForTest().activeDisposers).toBe(1);
    expect(service.snapshotInventoryForTest().size).toBe(0);
  });

  it('one owner callback tombstones every live state for its exact generation', () => {
    const { navigation, runtime } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    registerRender(service, navigation, attempt, reservationId());
    service.registerPrebidLease({
      reservationId: reservationId(1),
      slot: 'fictional-slot',
      navigation,
      auctionId: 'fictional-auction',
      adUnitCode: 'fictional-slot',
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1 },
      prebidBid: Object.freeze({ cpm: 1 }),
    });

    expect(navigation.snapshotInventoryForTest().activeDisposers).toBe(1);
    runtime.replaceNavigation();

    expect(service.recognize(reservationId())).toMatchObject({ state: 'disposed' });
    expect(service.recognize(reservationId(1))).toMatchObject({ state: 'aborted' });
    expect(service.snapshotInventoryForTest()).toMatchObject({ live: 0, tombstones: 2 });
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

  it('promotes one selected ADM lease from ten seconds to 15 minutes', () => {
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
    const selected = claim(service, navigation, attempt, reservationId(1));
    const winnerContext = attempt.winnerContext;
    if (!selected.recognized || !selected.claimed || !winnerContext) {
      throw new Error('Expected the promoted ADM lease to remain claimable');
    }
    expect(
      service.consumeClaim(selected, {
        attempt,
        attemptId: attempt.id,
        slot: attempt.slot,
        navigationGeneration: navigation.generation,
        winnerContext,
      })
    ).toEqual({ renderSource: admSource(), winnerContext });
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

  it.each(['aborted', 'prebid_selection_timeout', 'unselected'] as const)(
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
  it('preserves one ADM source and immutable context after registration input mutation', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    const source = admSource();
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
      prepareWinnerContext(winnerContext: WinnerContext) {
        const recognition = service.recognize(reservationId());
        if (recognition.recognized) observedStates.push(recognition.state);
        return attempt.prepareWinnerContext(winnerContext);
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
    const winnerContext = attempt.winnerContext;
    if (!result.recognized || !result.claimed || !winnerContext) {
      throw new Error('Expected one claimed ADM winner');
    }
    expect(
      service.consumeClaim(result, {
        attempt: sink,
        attemptId: attempt.id,
        slot: attempt.slot,
        navigationGeneration: navigation.generation,
        winnerContext,
      })
    ).toEqual({ renderSource: source, winnerContext });
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
      prepareWinnerContext(context: WinnerContext) {
        return {
          commit(): boolean {
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
          rollback(): boolean {
            if (acceptedContext === context) acceptedContext = undefined;
            return true;
          },
        };
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

  it('terminally suppresses a throwing context preparation without retaining PUC source', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    registerRender(service, navigation, attempt);
    const throwingSink = {
      id: attempt.id,
      slot: attempt.slot,
      winnerContext: undefined,
      isCurrent: () => true,
      prepareWinnerContext() {
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
    ).toEqual({ recognized: true, claimed: false, state: 'stale' });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 1,
      entriesWithPucSource: 0,
    });
  });

  it('terminally suppresses a claim when winner admission mutates, reenters, and throws', () => {
    const { navigation } = runtimeNavigation();
    const realAttempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    registerRender(service, navigation, realAttempt);
    const firstSource = Object.freeze({ name: 'first' });
    const secondSource = Object.freeze({ name: 'second' });
    let accepted: WinnerContext | undefined;
    let nested: ReturnType<typeof service.claim> | undefined;
    const attempt = {
      id: realAttempt.id,
      slot: realAttempt.slot,
      get winnerContext(): WinnerContext | undefined {
        return accepted;
      },
      isCurrent: () => true,
      prepareWinnerContext(context: WinnerContext) {
        return {
          commit(): boolean {
            accepted = context;
            nested = service.claim({
              reservationId: reservationId(),
              slot: realAttempt.slot,
              navigationGeneration: navigation.generation,
              attempt,
              pucSource: secondSource,
            });
            throw new Error('commit failed after mutation');
          },
          rollback(): boolean {
            if (accepted === context) accepted = undefined;
            return true;
          },
        };
      },
    };

    expect(
      service.claim({
        reservationId: reservationId(),
        slot: realAttempt.slot,
        navigationGeneration: navigation.generation,
        attempt,
        pucSource: firstSource,
      })
    ).toEqual({ recognized: true, claimed: false, state: 'stale' });
    expect(nested).toEqual({ recognized: true, claimed: false, state: 'renderable' });
    expect(accepted).toBeUndefined();
    expect(claim(service, navigation, realAttempt, reservationId(), secondSource)).toEqual({
      recognized: true,
      claimed: false,
      state: 'stale',
    });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 1,
      entriesWithPucSource: 0,
    });
  });

  it('terminally suppresses a Prebid promotion when winner admission has unknown postcondition', () => {
    const { navigation } = runtimeNavigation();
    const realAttempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    const bid = Object.freeze({ cpm: 1 });
    service.registerPrebidLease({
      reservationId: reservationId(),
      slot: realAttempt.slot,
      navigation,
      auctionId: 'fictional-auction',
      adUnitCode: realAttempt.slot,
      renderSource: admSource(),
      winnerContext: { selectedCpm: 1 },
      prebidBid: bid,
    });
    let accepted: WinnerContext | undefined;
    const attempt = {
      id: realAttempt.id,
      slot: realAttempt.slot,
      get winnerContext(): WinnerContext | undefined {
        throw new Error('winner context postcondition unavailable');
      },
      isCurrent: () => true,
      prepareWinnerContext(context: WinnerContext) {
        return {
          commit(): boolean {
            accepted = context;
            return true;
          },
          rollback(): boolean {
            if (accepted === context) accepted = undefined;
            return true;
          },
        };
      },
    };

    expect(
      service.promotePrebidSelection({
        reservationId: reservationId(),
        auctionId: 'fictional-auction',
        adUnitCode: realAttempt.slot,
        navigationGeneration: navigation.generation,
        attempt,
        prebidBid: bid,
      })
    ).toEqual({ ok: false, reason: 'invalid_attempt' });
    expect(accepted).toBeUndefined();
    expect(service.recognize(reservationId())).toMatchObject({ state: 'stale' });
    expect(
      service.promotePrebidSelection({
        reservationId: reservationId(),
        auctionId: 'fictional-auction',
        adUnitCode: realAttempt.slot,
        navigationGeneration: navigation.generation,
        attempt: realAttempt,
        prebidBid: bid,
      })
    ).toEqual({ ok: false, reason: 'reservation_not_live' });
  });

  it('uses captured freezing during a claim without retaining busy claim state', () => {
    const { navigation } = runtimeNavigation();
    const attempt = renderAttempt(navigation);
    const service = serviceAt(() => 0);
    registerRender(service, navigation, attempt);
    const pucSource = Object.freeze({});
    const originalFreeze = Object.freeze;
    let result: ReturnType<typeof service.claim> | undefined;
    let thrown: unknown;

    Object.freeze = function poisonedFreeze() {
      throw new Error('poisoned Object.freeze');
    };
    try {
      result = claim(service, navigation, attempt, reservationId(), pucSource);
    } catch (error) {
      thrown = error;
    } finally {
      Object.freeze = originalFreeze;
    }

    expect(thrown).toBeUndefined();
    expect(result).toMatchObject({ recognized: true, claimed: true });
    expect(service.recognize(reservationId())).toMatchObject({ state: 'consumed' });
    expect(service.snapshotInventoryForTest()).toMatchObject({
      live: 0,
      tombstones: 1,
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
