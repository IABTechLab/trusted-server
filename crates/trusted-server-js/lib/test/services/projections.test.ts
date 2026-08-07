import { describe, expect, it, vi } from 'vitest';

import { parseBrowserAuctionProjectionV1 } from '../../src/core/contracts/auction_projection';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';
import { createRuntimeSession, type NavigationSession } from '../../src/kernel/sessions';
import {
  createPageBidsController,
  prepareInitialAuctionProjection,
  type PreparedProjectionSlots,
  type ProjectionSlotRegistry,
} from '../../src/services/projections';

function runtimeSession() {
  let prefix = 0;
  return createRuntimeSession({
    createIdentityIssuer: () => {
      prefix += 1;
      return createTestNavigationIdentityIssuer({
        getRandomValues: (target) => {
          target.fill(prefix);
          return target;
        },
      });
    },
  });
}

function projection(slots: readonly string[], auctionId = 'page-bids') {
  return {
    version: 1,
    auction: {
      version: 1,
      auctionId,
      results: slots.map((slot) => ({ slot, outcome: 'no_bid' as const })),
    },
    bids: [],
  };
}

class SlotLedger implements ProjectionSlotRegistry {
  public readonly slots = new Set<string>();
  public prepareCalls = 0;
  public commitHook: (() => void) | undefined;

  public constructor(programmaticCount = 0) {
    for (let index = 0; index < programmaticCount; index += 1) {
      this.slots.add(`programmatic-${index}`);
    }
  }

  public prepareProjectionSlots(
    ownerGeneration: object,
    slots: readonly string[],
    maximumActiveSlots: number
  ): PreparedProjectionSlots | undefined {
    this.prepareCalls += 1;
    if (
      this.slots.size + slots.length > maximumActiveSlots ||
      slots.some((slot) => this.slots.has(slot))
    ) {
      return undefined;
    }
    let committed = false;
    return Object.freeze({
      ownerGeneration,
      commit: () => {
        this.commitHook?.();
        for (const slot of slots) this.slots.add(slot);
        committed = true;
        return true;
      },
      rollback: () => {
        if (!committed) return;
        for (const slot of slots) this.slots.delete(slot);
        committed = false;
      },
    });
  }
}

function controller(navigation: NavigationSession, registry: ProjectionSlotRegistry) {
  return createPageBidsController({
    navigation,
    parseProjection: parseBrowserAuctionProjectionV1,
    slotRegistry: registry,
  });
}

describe('initial auction projection', () => {
  it('deep-copies and recursively freezes boot input without mutating it', () => {
    const bootProjection = projection(['server-slot'], 'initial');

    const prepared = prepareInitialAuctionProjection(
      bootProjection,
      parseBrowserAuctionProjectionV1
    );

    expect(prepared).toEqual(bootProjection);
    expect(prepared).not.toBe(bootProjection);
    expect(Object.isFrozen(prepared)).toBe(true);
    expect(Object.isFrozen((prepared as typeof bootProjection).auction)).toBe(true);
    expect(Object.isFrozen((prepared as typeof bootProjection).auction.results)).toBe(true);
    expect(Object.isFrozen(bootProjection)).toBe(false);
    bootProjection.auction.auctionId = 'publisher-mutated';
    expect((prepared as typeof bootProjection).auction.auctionId).toBe('initial');
  });
});

describe('SPA page-bids projection controller', () => {
  it('atomically reserves slots and commits one immutable current-generation projection', () => {
    const runtime = runtimeSession();
    const navigation = runtime.startInitialNavigation(
      prepareInitialAuctionProjection(projection([], 'initial'), parseBrowserAuctionProjectionV1)
    );
    if (!navigation.ok) throw new Error('Expected initial navigation');
    const spa = runtime.replaceNavigation();
    if (!spa.ok) throw new Error('Expected SPA navigation');
    const registry = new SlotLedger(254);
    const input = projection(['server-one', 'server-two']);

    expect(controller(spa.value, registry).commit(input)).toEqual({ status: 'committed' });
    expect([...registry.slots].slice(-2)).toEqual(['server-one', 'server-two']);
    expect(spa.value.currentAuctionProjection).toEqual(input);
    expect(spa.value.currentAuctionProjection).not.toBe(input);
    expect(Object.isFrozen(spa.value.currentAuctionProjection)).toBe(true);
    expect(
      Object.isFrozen((spa.value.currentAuctionProjection as typeof input).auction.results[0])
    ).toBe(true);
    input.auction.auctionId = 'publisher-mutated';
    expect((spa.value.currentAuctionProjection as typeof input).auction.auctionId).toBe(
      'page-bids'
    );
  });

  it('rejects a duplicate response without preparing or changing committed state', () => {
    const runtime = runtimeSession();
    const spa = runtime.startInitialNavigation();
    if (!spa.ok) throw new Error('Expected navigation');
    const registry = new SlotLedger();
    const pageBids = controller(spa.value, registry);

    expect(pageBids.commit(projection(['first']))).toEqual({ status: 'committed' });
    expect(pageBids.commit(projection(['second']))).toEqual({
      status: 'rejected',
      reason: 'duplicate',
    });
    expect(registry.prepareCalls).toBe(1);
    expect([...registry.slots]).toEqual(['first']);
    expect(
      (spa.value.currentAuctionProjection as ReturnType<typeof projection>).auction.results
    ).toEqual([{ slot: 'first', outcome: 'no_bid' }]);
  });

  it('makes a late old-generation response inert after navigation replacement', () => {
    const runtime = runtimeSession();
    const initial = runtime.startInitialNavigation();
    if (!initial.ok) throw new Error('Expected navigation');
    const registry = new SlotLedger();
    const pageBids = controller(initial.value, registry);
    const replacement = runtime.replaceNavigation();
    if (!replacement.ok) throw new Error('Expected replacement');

    expect(pageBids.commit(projection(['stale']))).toEqual({
      status: 'rejected',
      reason: 'stale',
    });
    expect(registry.prepareCalls).toBe(0);
    expect(registry.slots.size).toBe(0);
    expect(replacement.value.currentAuctionProjection).toBeUndefined();
  });

  it('rejects malformed input without retaining or reserving it', () => {
    const runtime = runtimeSession();
    const navigation = runtime.startInitialNavigation();
    if (!navigation.ok) throw new Error('Expected navigation');
    const registry = new SlotLedger();
    const malformed = { ...projection(['slot']), extra: true };

    expect(controller(navigation.value, registry).commit(malformed)).toEqual({
      status: 'rejected',
      reason: 'malformed',
    });
    expect(registry.prepareCalls).toBe(0);
    expect(registry.slots.size).toBe(0);
    expect(navigation.value.currentAuctionProjection).toBeUndefined();
  });

  it.each([
    [255, 1, 'committed'],
    [255, 2, 'capacity'],
    [256, 1, 'capacity'],
  ] as const)(
    'enforces the shared 256 cap with %i programmatic plus %i projected slots',
    (programmatic, projected, expected) => {
      const runtime = runtimeSession();
      const navigation = runtime.startInitialNavigation();
      if (!navigation.ok) throw new Error('Expected navigation');
      const registry = new SlotLedger(programmatic);
      const slots = Array.from({ length: projected }, (_, index) => `server-${index}`);

      const result = controller(navigation.value, registry).commit(projection(slots));

      expect(result).toEqual(
        expected === 'committed'
          ? { status: 'committed' }
          : { status: 'rejected', reason: 'capacity' }
      );
      expect(registry.slots.size).toBe(expected === 'committed' ? 256 : programmatic);
      expect(navigation.value.currentAuctionProjection === undefined).toBe(
        expected !== 'committed'
      );
    }
  );

  it('rolls back prepared slots if ownership changes during the synchronous commit', () => {
    const runtime = runtimeSession();
    const navigation = runtime.startInitialNavigation();
    if (!navigation.ok) throw new Error('Expected navigation');
    const registry = new SlotLedger();
    registry.commitHook = () => {
      runtime.replaceNavigation();
    };

    expect(controller(navigation.value, registry).commit(projection(['raced']))).toEqual({
      status: 'rejected',
      reason: 'stale',
    });
    expect(registry.slots.size).toBe(0);
    expect(runtime.currentNavigation?.currentAuctionProjection).toBeUndefined();
  });

  it('does not retain prior-navigation projection after a malformed SPA response', () => {
    const runtime = runtimeSession();
    const initialProjection = prepareInitialAuctionProjection(
      projection(['old-slot'], 'initial'),
      parseBrowserAuctionProjectionV1
    );
    const initial = runtime.startInitialNavigation(initialProjection);
    if (!initial.ok) throw new Error('Expected initial navigation');
    const spa = runtime.replaceNavigation();
    if (!spa.ok) throw new Error('Expected SPA navigation');

    expect(controller(spa.value, new SlotLedger()).commit({ invalid: true })).toEqual({
      status: 'rejected',
      reason: 'malformed',
    });
    expect(initial.value.currentAuctionProjection).toBeUndefined();
    expect(spa.value.currentAuctionProjection).toBeUndefined();
  });

  it('isolates a throwing parser and a throwing reservation commit', () => {
    const runtime = runtimeSession();
    const first = runtime.startInitialNavigation();
    if (!first.ok) throw new Error('Expected navigation');
    const parser = vi.fn(() => {
      throw new Error('hostile parser');
    });
    expect(
      createPageBidsController({
        navigation: first.value,
        parseProjection: parser,
        slotRegistry: new SlotLedger(),
      }).commit(projection(['slot']))
    ).toEqual({ status: 'rejected', reason: 'malformed' });

    const second = runtime.replaceNavigation();
    if (!second.ok) throw new Error('Expected replacement');
    const rollback = vi.fn();
    const throwingRegistry: ProjectionSlotRegistry = {
      prepareProjectionSlots: () => ({
        ownerGeneration: second.value.generation,
        commit: () => {
          throw new Error('commit failed');
        },
        rollback,
      }),
    };
    expect(controller(second.value, throwingRegistry).commit(projection(['slot']))).toEqual({
      status: 'rejected',
      reason: 'capacity',
    });
    expect(rollback).toHaveBeenCalledOnce();
    expect(second.value.currentAuctionProjection).toBeUndefined();
  });
});
