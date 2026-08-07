import { describe, expect, it, vi } from 'vitest';

import {
  createAuctionContextRegistry,
  type ContextContributorOwner,
} from '../../src/services/context';

function owner(): ContextContributorOwner & { readonly dispose: () => void } {
  const generation = Object.freeze({});
  const disposers: (() => void)[] = [];
  let current = true;
  return Object.freeze({
    generation,
    isCurrent: () => current,
    onDispose: (_kind: string, callback: () => void) => {
      if (!current) callback();
      else disposers.push(callback);
    },
    dispose: () => {
      if (!current) return;
      current = false;
      for (let index = disposers.length - 1; index >= 0; index -= 1) {
        disposers[index]?.();
      }
      disposers.length = 0;
    },
  });
}

describe('AuctionContextRegistry', () => {
  it('snapshots in manifest order with later-key precedence and recursive freezing', () => {
    const runtimeOwner = owner();
    const firstOwner = owner();
    const secondOwner = owner();
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['first', 'second']),
      runtimeOwner,
    });

    expect(
      registry.register('second', () => ({ shared: 'second', nested: { value: 2 } }), secondOwner)
    ).toBe(true);
    expect(registry.register('first', () => ({ first: true, shared: 'first' }), firstOwner)).toBe(
      true
    );

    const snapshot = registry.snapshot();

    expect(snapshot).toEqual({ first: true, shared: 'second', nested: { value: 2 } });
    expect(Object.keys(snapshot)).toEqual(['first', 'shared', 'nested']);
    expect(Object.isFrozen(snapshot)).toBe(true);
    expect(Object.isFrozen(snapshot.nested)).toBe(true);
  });

  it('isolates a throwing contributor and does not retain any of its partial values', () => {
    const runtimeOwner = owner();
    const failure = vi.fn();
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['good-first', 'hostile', 'good-last']),
      runtimeOwner,
      onContributorFailure: failure,
    });
    const partial = { leaked: 'must-not-escape' };
    Object.defineProperty(partial, 'throwing', {
      enumerable: true,
      get() {
        throw new Error('hostile getter');
      },
    });
    registry.register('good-first', () => ({ retained: 'first' }), owner());
    registry.register('hostile', () => partial, owner());
    registry.register('good-last', () => ({ retained: 'last' }), owner());

    const snapshot = registry.snapshot();

    expect(snapshot).toEqual({ retained: 'last' });
    expect(snapshot).not.toHaveProperty('leaked');
    expect(failure.mock.calls).toEqual([
      [{ integrationId: 'hostile', reason: 'contributor_failed' }],
    ]);
    expect(Object.isFrozen(failure.mock.calls[0]?.[0])).toBe(true);
  });

  it('removes an owner-scoped contributor before the next batch snapshot', () => {
    const runtimeOwner = owner();
    const contributorOwner = owner();
    const contributor = vi.fn(() => ({ active: true }));
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner,
    });
    expect(registry.register('integration', contributor, contributorOwner)).toBe(true);
    expect(registry.snapshot()).toEqual({ active: true });

    contributorOwner.dispose();

    expect(registry.snapshot()).toEqual({});
    expect(contributor).toHaveBeenCalledOnce();
    expect(registry.snapshotInventoryForTest()).toEqual({
      disposed: false,
      registrations: [],
    });
  });

  it('rejects unknown, duplicate, and stale-owner registrations', () => {
    const runtimeOwner = owner();
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['known']),
      runtimeOwner,
    });
    const active = owner();
    const stale = owner();
    stale.dispose();

    expect(registry.register('unknown', () => ({}), active)).toBe(false);
    expect(registry.register('known', () => ({ first: true }), active)).toBe(true);
    expect(registry.register('known', () => ({ duplicate: true }), owner())).toBe(false);
    active.dispose();
    expect(registry.register('known', () => ({ stale: true }), stale)).toBe(false);
  });

  it('takes one fresh contributor snapshot per batch call without retaining prior values', () => {
    const runtimeOwner = owner();
    const mutable = { value: 1 };
    const contributor = vi.fn(() => ({ nested: mutable }));
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner,
    });
    registry.register('integration', contributor, owner());

    const first = registry.snapshot();
    mutable.value = 2;
    const second = registry.snapshot();

    expect(first).toEqual({ nested: { value: 1 } });
    expect(second).toEqual({ nested: { value: 2 } });
    expect(first).not.toBe(second);
    expect(first.nested).not.toBe(second.nested);
    expect(contributor).toHaveBeenCalledTimes(2);
  });

  it('makes stale callbacks and logger failures inert after runtime disposal', () => {
    const runtimeOwner = owner();
    const contributor = vi.fn(() => {
      throw new Error('contributor failed');
    });
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner,
      onContributorFailure: () => {
        throw new Error('logger failed');
      },
    });
    registry.register('integration', contributor, owner());

    expect(() => registry.snapshot()).not.toThrow();
    runtimeOwner.dispose();
    expect(registry.snapshot()).toEqual({});
    expect(contributor).toHaveBeenCalledOnce();
    expect(registry.snapshotInventoryForTest()).toEqual({
      disposed: true,
      registrations: [],
    });
  });

  it('discards the whole batch snapshot if a contributor disposes the runtime', () => {
    const runtimeOwner = owner();
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['first', 'disposing']),
      runtimeOwner,
    });
    registry.register('first', () => ({ stale: 'must-not-escape' }), owner());
    registry.register(
      'disposing',
      () => {
        runtimeOwner.dispose();
        return { late: 'must-not-escape' };
      },
      owner()
    );

    expect(registry.snapshot()).toEqual({});
  });

  it('fails closed for a manifest beyond the integration bound', () => {
    const ids = Object.freeze(Array.from({ length: 17 }, (_, index) => `integration-${index}`));

    expect(() =>
      createAuctionContextRegistry({ manifestIntegrationIds: ids, runtimeOwner: owner() })
    ).toThrow(TypeError);
  });
});
