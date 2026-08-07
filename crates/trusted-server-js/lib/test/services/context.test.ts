import { describe, expect, it, vi } from 'vitest';

import {
  createAuctionContextRegistry,
  type ContextContributorOwner,
} from '../../src/services/context';

const MAX_CONTEXT_JSON_BYTES = 256 * 1024;
const MAX_CONTEXT_ENCODED_KEY_BYTES = MAX_CONTEXT_JSON_BYTES - 7;
const MAX_CONTEXT_STRUCTURE_ENTRIES = Math.floor((MAX_CONTEXT_JSON_BYTES - 1) / 2);

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

  it('fails closed when the runtime-owner generation getter throws during construction', () => {
    const hostileRuntimeOwner = new Proxy(owner(), {
      get(target, key, receiver) {
        if (key === 'generation') throw new Error('hostile generation getter');
        return Reflect.get(target, key, receiver);
      },
    });
    let registry: ReturnType<typeof createAuctionContextRegistry> | undefined;

    expect(() => {
      registry = createAuctionContextRegistry({
        manifestIntegrationIds: Object.freeze(['integration']),
        runtimeOwner: hostileRuntimeOwner,
      });
    }).not.toThrow();

    const snapshot = registry?.snapshot();
    expect(snapshot).toEqual({});
    expect(Object.isFrozen(snapshot)).toBe(true);
    expect(registry?.register('integration', () => ({ leaked: true }), owner())).toBe(false);
    expect(registry?.snapshotInventoryForTest()).toEqual({
      disposed: true,
      registrations: [],
    });
  });

  it('fails closed when the runtime-owner generation getter throws during a later snapshot', () => {
    const runtimeOwner = owner();
    let throwOnGenerationRead = false;
    const hostileRuntimeOwner = new Proxy(runtimeOwner, {
      get(target, key, receiver) {
        if (key === 'generation' && throwOnGenerationRead) {
          throw new Error('hostile generation getter');
        }
        return Reflect.get(target, key, receiver);
      },
    });
    const contributor = vi.fn(() => ({ leaked: true }));
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner: hostileRuntimeOwner,
    });
    expect(registry.register('integration', contributor, owner())).toBe(true);

    throwOnGenerationRead = true;
    let snapshot: Readonly<Record<string, unknown>> | undefined;
    expect(() => {
      snapshot = registry.snapshot();
    }).not.toThrow();

    expect(snapshot).toEqual({});
    expect(Object.isFrozen(snapshot)).toBe(true);
    expect(contributor).not.toHaveBeenCalled();
    expect(registry.snapshotInventoryForTest()).toEqual({
      disposed: true,
      registrations: [],
    });
  });

  it('contains a throwing contributor-owner generation getter without retention', () => {
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner: owner(),
    });
    const hostileOwner = new Proxy(owner(), {
      get(target, key, receiver) {
        if (key === 'generation') throw new Error('hostile generation getter');
        return Reflect.get(target, key, receiver);
      },
    });

    expect(() =>
      registry.register('integration', () => ({ leaked: true }), hostileOwner)
    ).not.toThrow();
    expect(registry.snapshotInventoryForTest()).toEqual({
      disposed: false,
      registrations: [],
    });
    expect(registry.snapshot()).toEqual({});
  });

  it('reads contributor-owner generation once at each registration checkpoint', () => {
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner: owner(),
    });
    const generation = Object.freeze({});
    const readGeneration = vi.fn(() => generation);
    const contributorOwner = {
      get generation() {
        return readGeneration();
      },
      isCurrent: () => true,
      onDispose: vi.fn(),
    };

    expect(registry.register('integration', () => ({ retained: true }), contributorOwner)).toBe(
      true
    );
    expect(readGeneration).toHaveBeenCalledTimes(2);
    expect(registry.snapshotInventoryForTest()).toEqual({
      disposed: false,
      registrations: ['integration'],
    });
  });

  it.each([
    ['finalization', false],
    ['throw rollback', true],
  ] as const)(
    'does not delete a reentrant replacement record during outer %s',
    (_name, throwAfterReplacement) => {
      const registry = createAuctionContextRegistry({
        manifestIntegrationIds: Object.freeze(['integration']),
        runtimeOwner: owner(),
      });
      const replacementOwner = owner();
      let replacementRegistered: boolean | undefined;
      const outerOwner: ContextContributorOwner = {
        generation: Object.freeze({}),
        isCurrent: () => true,
        onDispose: (_kind, cleanup) => {
          cleanup();
          replacementRegistered = registry.register(
            'integration',
            () => ({ replacement: true }),
            replacementOwner
          );
          if (throwAfterReplacement) throw new Error('outer onDispose failed');
        },
      };

      expect(registry.register('integration', () => ({ outer: true }), outerOwner)).toBe(false);
      expect(replacementRegistered).toBe(true);
      expect(registry.snapshotInventoryForTest()).toEqual({
        disposed: false,
        registrations: ['integration'],
      });
      expect(registry.snapshot()).toEqual({ replacement: true });
    }
  );

  it('reports a registration as displaced when final owner reflection installs a replacement', () => {
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner: owner(),
    });
    const generation = Object.freeze({});
    const replacementOwner = owner();
    let generationReads = 0;
    let cleanup: (() => void) | undefined;
    let replacementRegistered: boolean | undefined;
    const reentrantOwner: ContextContributorOwner = {
      get generation() {
        generationReads += 1;
        if (generationReads === 2) {
          cleanup?.();
          replacementRegistered = registry.register(
            'integration',
            () => ({ replacement: true }),
            replacementOwner
          );
        }
        return generation;
      },
      isCurrent: () => true,
      onDispose: (_kind, callback) => {
        cleanup = callback;
      },
    };

    expect(registry.register('integration', () => ({ displaced: true }), reentrantOwner)).toBe(
      false
    );
    expect(replacementRegistered).toBe(true);
    expect(registry.snapshotInventoryForTest()).toEqual({
      disposed: false,
      registrations: ['integration'],
    });
    expect(registry.snapshot()).toEqual({ replacement: true });
  });

  it('rolls back a registration whose owner generation changes during onDispose', () => {
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner: owner(),
    });
    const firstGeneration = Object.freeze({});
    const secondGeneration = Object.freeze({});
    let generation = firstGeneration;
    let rotateGeneration = true;
    const readGeneration = vi.fn(() => generation);
    const changingOwner: ContextContributorOwner = {
      get generation() {
        return readGeneration();
      },
      isCurrent: () => true,
      onDispose: () => {
        if (!rotateGeneration) return;
        rotateGeneration = false;
        generation = secondGeneration;
      },
    };

    expect(registry.register('integration', () => ({ stale: true }), changingOwner)).toBe(false);
    expect(registry.snapshotInventoryForTest()).toEqual({
      disposed: false,
      registrations: [],
    });
    expect(registry.register('integration', () => ({ current: true }), changingOwner)).toBe(true);
    expect(readGeneration).toHaveBeenCalledTimes(4);
    expect(registry.snapshot()).toEqual({ current: true });
  });

  it('rejects a reflected contributor-owner generation that is not an object', () => {
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner: owner(),
    });
    const invalidOwner = {
      generation: null as unknown as object,
      isCurrent: () => true,
      onDispose: vi.fn(),
    };

    expect(registry.register('integration', () => ({ leaked: true }), invalidOwner)).toBe(false);
    expect(registry.snapshotInventoryForTest()).toEqual({
      disposed: false,
      registrations: [],
    });
  });

  it.each(['isCurrent', 'onDispose'] as const)(
    'contains a throwing contributor-owner %s trap without retention',
    (method) => {
      const registry = createAuctionContextRegistry({
        manifestIntegrationIds: Object.freeze(['integration']),
        runtimeOwner: owner(),
      });
      const contributorOwner = owner();
      const hostileOwner = new Proxy(contributorOwner, {
        get(target, key, receiver) {
          if (key === method) throw new Error(`hostile ${method} trap`);
          return Reflect.get(target, key, receiver);
        },
      });

      expect(() =>
        registry.register('integration', () => ({ leaked: true }), hostileOwner)
      ).not.toThrow();
      expect(registry.snapshotInventoryForTest()).toEqual({
        disposed: false,
        registrations: [],
      });
    }
  );

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

  it('does not invoke a record displaced during its owner-currentness reflection', () => {
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner: owner(),
    });
    const generation = Object.freeze({});
    const replacementOwner = owner();
    const displacedContributor = vi.fn(() => ({ displaced: true }));
    const replacementContributor = vi.fn(() => ({ replacement: true }));
    let generationReads = 0;
    let cleanup: (() => void) | undefined;
    let replacementRegistered: boolean | undefined;
    const reentrantOwner: ContextContributorOwner = {
      get generation() {
        generationReads += 1;
        if (generationReads === 3) {
          cleanup?.();
          replacementRegistered = registry.register(
            'integration',
            replacementContributor,
            replacementOwner
          );
        }
        return generation;
      },
      isCurrent: () => true,
      onDispose: (_kind, callback) => {
        cleanup = callback;
      },
    };
    expect(registry.register('integration', displacedContributor, reentrantOwner)).toBe(true);

    expect(registry.snapshot()).toEqual({});
    expect(replacementRegistered).toBe(true);
    expect(displacedContributor).not.toHaveBeenCalled();
    expect(replacementContributor).not.toHaveBeenCalled();
    expect(registry.snapshot()).toEqual({ replacement: true });
    expect(replacementContributor).toHaveBeenCalledOnce();
  });

  it('does not merge a record displaced during contributor execution', () => {
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner: owner(),
    });
    const generation = Object.freeze({});
    const replacementOwner = owner();
    const replacementContributor = vi.fn(() => ({ replacement: true }));
    let cleanup: (() => void) | undefined;
    let replacementRegistered: boolean | undefined;
    const reentrantOwner: ContextContributorOwner = {
      generation,
      isCurrent: () => true,
      onDispose: (_kind, callback) => {
        cleanup = callback;
      },
    };
    const displacedContributor = vi.fn(() => {
      cleanup?.();
      replacementRegistered = registry.register(
        'integration',
        replacementContributor,
        replacementOwner
      );
      return { displaced: true };
    });
    expect(registry.register('integration', displacedContributor, reentrantOwner)).toBe(true);

    expect(registry.snapshot()).toEqual({});
    expect(replacementRegistered).toBe(true);
    expect(displacedContributor).toHaveBeenCalledOnce();
    expect(replacementContributor).not.toHaveBeenCalled();
    expect(registry.snapshot()).toEqual({ replacement: true });
    expect(replacementContributor).toHaveBeenCalledOnce();
  });

  it('does not merge a record displaced during runtime-currentness reflection', () => {
    const runtimeGeneration = Object.freeze({});
    let replaceOnRuntimeReflection = false;
    let contributorCleanup: (() => void) | undefined;
    let replacementRegistered: boolean | undefined;
    const replacementOwner = owner();
    const replacementContributor = vi.fn(() => ({ replacement: true }));
    const runtimeOwner: ContextContributorOwner = {
      get generation() {
        if (replaceOnRuntimeReflection) {
          replaceOnRuntimeReflection = false;
          contributorCleanup?.();
          replacementRegistered = registry.register(
            'integration',
            replacementContributor,
            replacementOwner
          );
        }
        return runtimeGeneration;
      },
      isCurrent: () => true,
      onDispose: vi.fn(),
    };
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner,
    });
    const displacedContributor = vi.fn(() => {
      replaceOnRuntimeReflection = true;
      return { displaced: true };
    });
    expect(
      registry.register('integration', displacedContributor, {
        generation: Object.freeze({}),
        isCurrent: () => true,
        onDispose: (_kind, callback) => {
          contributorCleanup = callback;
        },
      })
    ).toBe(true);

    expect(registry.snapshot()).toEqual({});
    expect(replacementRegistered).toBe(true);
    expect(displacedContributor).toHaveBeenCalledOnce();
    expect(replacementContributor).not.toHaveBeenCalled();
    expect(registry.snapshot()).toEqual({ replacement: true });
    expect(replacementContributor).toHaveBeenCalledOnce();
  });

  it('fails closed when final runtime reflection displaces an already accepted record', () => {
    const runtimeGeneration = Object.freeze({});
    const contributorGeneration = Object.freeze({});
    const replacementOwner = owner();
    const staleContributor = vi.fn(() => ({ stale: true }));
    const replacementContributor = vi.fn(() => ({ replacement: true }));
    let contributorGenerationReads = 0;
    let contributorCleanup: (() => void) | undefined;
    let reflectReplacement = false;
    let replacementRegistered: boolean | undefined;
    const runtimeOwner: ContextContributorOwner = {
      get generation() {
        if (reflectReplacement) {
          reflectReplacement = false;
          contributorCleanup?.();
          replacementRegistered = registry.register(
            'integration',
            replacementContributor,
            replacementOwner
          );
        }
        return runtimeGeneration;
      },
      isCurrent: () => true,
      onDispose: vi.fn(),
    };
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner,
    });
    expect(
      registry.register('integration', staleContributor, {
        get generation() {
          contributorGenerationReads += 1;
          if (contributorGenerationReads === 4) reflectReplacement = true;
          return contributorGeneration;
        },
        isCurrent: () => true,
        onDispose: (_kind, callback) => {
          contributorCleanup = callback;
        },
      })
    ).toBe(true);

    const firstSnapshot = registry.snapshot();
    expect(firstSnapshot).toEqual({});
    expect(Object.isFrozen(firstSnapshot)).toBe(true);
    expect(replacementRegistered).toBe(true);
    expect(staleContributor).toHaveBeenCalledOnce();
    expect(replacementContributor).not.toHaveBeenCalled();
    expect(registry.snapshotInventoryForTest()).toEqual({
      disposed: false,
      registrations: ['integration'],
    });
    expect(registry.snapshot()).toEqual({ replacement: true });
    expect(replacementContributor).toHaveBeenCalledOnce();
  });

  it('fails closed when final runtime reflection disposes the registry after acceptance', () => {
    const runtimeGeneration = Object.freeze({});
    const contributorGeneration = Object.freeze({});
    let contributorGenerationReads = 0;
    let reflectDisposal = false;
    const runtimeOwner: ContextContributorOwner = {
      get generation() {
        if (reflectDisposal) {
          reflectDisposal = false;
          registry.dispose();
        }
        return runtimeGeneration;
      },
      isCurrent: () => true,
      onDispose: vi.fn(),
    };
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner,
    });
    expect(
      registry.register('integration', () => ({ stale: true }), {
        get generation() {
          contributorGenerationReads += 1;
          if (contributorGenerationReads === 4) reflectDisposal = true;
          return contributorGeneration;
        },
        isCurrent: () => true,
        onDispose: vi.fn(),
      })
    ).toBe(true);

    const snapshot = registry.snapshot();
    expect(snapshot).toEqual({});
    expect(Object.isFrozen(snapshot)).toBe(true);
    expect(registry.snapshotInventoryForTest()).toEqual({
      disposed: true,
      registrations: [],
    });
  });

  it('does not classify primitive clone records through Object.prototype pollution', () => {
    const priorDescriptor = Object.getOwnPropertyDescriptor(Object.prototype, 'source');
    try {
      Object.defineProperty(Object.prototype, 'source', {
        configurable: true,
        enumerable: false,
        value: 'polluted',
        writable: true,
      });
      const registry = createAuctionContextRegistry({
        manifestIntegrationIds: Object.freeze(['integration']),
        runtimeOwner: owner(),
      });
      registry.register(
        'integration',
        () => ({ string: 'value', number: 7, boolean: true, nullable: null }),
        owner()
      );

      expect(registry.snapshot()).toEqual({
        string: 'value',
        number: 7,
        boolean: true,
        nullable: null,
      });
    } finally {
      if (priorDescriptor) Object.defineProperty(Object.prototype, 'source', priorDescriptor);
      else Reflect.deleteProperty(Object.prototype, 'source');
    }
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

  it.each([
    ['just below', MAX_CONTEXT_JSON_BYTES - 1, true],
    ['at', MAX_CONTEXT_JSON_BYTES, true],
    ['above', MAX_CONTEXT_JSON_BYTES + 1, false],
  ] as const)(
    'applies the shared JSON byte budget %s the body ceiling',
    (_name, bytes, accepted) => {
      const failure = vi.fn();
      const registry = createAuctionContextRegistry({
        manifestIntegrationIds: Object.freeze(['integration']),
        runtimeOwner: owner(),
        onContributorFailure: failure,
      });
      const payload = 'x'.repeat(bytes - 14);
      registry.register('integration', () => ({ payload }), owner());

      const snapshot = registry.snapshot();

      if (accepted) {
        expect(new TextEncoder().encode(JSON.stringify(snapshot)).byteLength).toBe(bytes);
        expect(snapshot).toEqual({ payload });
        expect(failure).not.toHaveBeenCalled();
      } else {
        expect(snapshot).toEqual({});
        expect(failure.mock.calls).toEqual([
          [{ integrationId: 'integration', reason: 'contributor_failed' }],
        ]);
      }
    }
  );

  it('accounts for multibyte and escaped JSON strings at the exact byte ceiling', () => {
    const payloadBytes = MAX_CONTEXT_JSON_BYTES - 14;
    const emojiCount = Math.floor((payloadBytes - 4) / 4);
    const payload = `${'😀'.repeat(emojiCount)}xx"\n`;
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['integration']),
      runtimeOwner: owner(),
    });
    registry.register('integration', () => ({ payload }), owner());

    const snapshot = registry.snapshot();

    expect(new TextEncoder().encode(JSON.stringify(snapshot)).byteLength).toBe(
      MAX_CONTEXT_JSON_BYTES
    );
    expect(snapshot).toEqual({ payload });
  });

  it('shares the byte budget across contributors and rejects an overflowing merge atomically', () => {
    const failure = vi.fn();
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['first', 'overflowing']),
      runtimeOwner: owner(),
      onContributorFailure: failure,
    });
    const payload = 'x'.repeat(MAX_CONTEXT_JSON_BYTES - 14);
    registry.register('first', () => ({ payload }), owner());
    registry.register('overflowing', () => ({ late: true }), owner());

    const snapshot = registry.snapshot();

    expect(new TextEncoder().encode(JSON.stringify(snapshot)).byteLength).toBe(
      MAX_CONTEXT_JSON_BYTES
    );
    expect(snapshot).toEqual({ payload });
    expect(failure.mock.calls).toEqual([
      [{ integrationId: 'overflowing', reason: 'contributor_failed' }],
    ]);
  });

  it('subtracts replaced predecessor bytes before admitting a later contributor', () => {
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['first', 'replacement']),
      runtimeOwner: owner(),
    });
    registry.register(
      'first',
      () => ({ shared: 'x'.repeat(MAX_CONTEXT_JSON_BYTES - 13) }),
      owner()
    );
    registry.register('replacement', () => ({ shared: 'small', later: true }), owner());

    expect(registry.snapshot()).toEqual({ shared: 'small', later: true });
  });

  it('retains no replacement values when one prospective contributor exceeds the budget', () => {
    const failure = vi.fn();
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['first', 'overflowing']),
      runtimeOwner: owner(),
      onContributorFailure: failure,
    });
    registry.register('first', () => ({ shared: 'original' }), owner());
    registry.register(
      'overflowing',
      () => ({ shared: 'must-not-replace', excess: 'x'.repeat(MAX_CONTEXT_JSON_BYTES) }),
      owner()
    );

    expect(registry.snapshot()).toEqual({ shared: 'original' });
    expect(failure.mock.calls).toEqual([
      [{ integrationId: 'overflowing', reason: 'contributor_failed' }],
    ]);
  });

  it('clones and freezes a deeply nested contribution without a recursion cap', () => {
    const depth = 12_000;
    let deep: Record<string, unknown> = { terminal: true };
    for (let index = 0; index < depth; index += 1) deep = { next: deep };
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['deep']),
      runtimeOwner: owner(),
    });
    registry.register('deep', () => ({ deep }), owner());

    const snapshot = registry.snapshot();

    let cursor = snapshot.deep;
    for (let index = 0; index < depth; index += 1) {
      expect(Object.isFrozen(cursor)).toBe(true);
      cursor = (cursor as { readonly next: unknown }).next;
    }
    expect(cursor).toEqual({ terminal: true });
  });

  it('rejects an oversized encoded key before retaining contributor values', () => {
    const failure = vi.fn();
    const hugeKey = 'k'.repeat(MAX_CONTEXT_ENCODED_KEY_BYTES + 1);
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['huge-key']),
      runtimeOwner: owner(),
      onContributorFailure: failure,
    });
    registry.register('huge-key', () => ({ [hugeKey]: 'must-not-escape' }), owner());

    expect(registry.snapshot()).toEqual({});
    expect(failure.mock.calls).toEqual([
      [{ integrationId: 'huge-key', reason: 'contributor_failed' }],
    ]);
  });

  it('rejects a huge iterative structure and continues with the next contributor', () => {
    let huge: unknown[] = [];
    const depth = Math.ceil(MAX_CONTEXT_STRUCTURE_ENTRIES / 2) + 1;
    for (let index = 0; index < depth; index += 1) huge = [huge];
    const failure = vi.fn();
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['huge', 'later']),
      runtimeOwner: owner(),
      onContributorFailure: failure,
    });
    registry.register('huge', () => ({ huge }), owner());
    registry.register('later', () => ({ retained: true }), owner());

    expect(registry.snapshot()).toEqual({ retained: true });
    expect(failure.mock.calls).toEqual([[{ integrationId: 'huge', reason: 'contributor_failed' }]]);
  });

  it('rejects a cyclic graph atomically and continues in manifest order', () => {
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;
    const failure = vi.fn();
    const registry = createAuctionContextRegistry({
      manifestIntegrationIds: Object.freeze(['cyclic', 'later']),
      runtimeOwner: owner(),
      onContributorFailure: failure,
    });
    registry.register('cyclic', () => ({ leaked: true, cyclic }), owner());
    registry.register('later', () => ({ retained: true }), owner());

    expect(registry.snapshot()).toEqual({ retained: true });
    expect(failure.mock.calls).toEqual([
      [{ integrationId: 'cyclic', reason: 'contributor_failed' }],
    ]);
  });

  it('fails closed for a manifest beyond the integration bound', () => {
    const ids = Object.freeze(Array.from({ length: 17 }, (_, index) => `integration-${index}`));

    expect(() =>
      createAuctionContextRegistry({ manifestIntegrationIds: ids, runtimeOwner: owner() })
    ).toThrow(TypeError);
  });
});
