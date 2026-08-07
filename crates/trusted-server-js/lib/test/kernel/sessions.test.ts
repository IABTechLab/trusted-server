import { describe, expect, it, vi } from 'vitest';

import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';
import {
  createRuntimeSession,
  type NavigationIdentityIssuerFactory,
} from '../../src/kernel/sessions';

function identityFactory(seed = 1): NavigationIdentityIssuerFactory {
  let navigation = seed;
  return () => {
    const value = navigation;
    navigation += 1;
    return createTestNavigationIdentityIssuer({
      getRandomValues: (target) => {
        target.fill(value);
        return target;
      },
    });
  };
}

function frozenProjection(id: string): Readonly<object> {
  return Object.freeze({
    version: 1,
    auction: Object.freeze({ version: 1, auctionId: id, results: Object.freeze([]) }),
    bids: Object.freeze([]),
  });
}

describe('runtime and navigation sessions', () => {
  it('owns one current navigation and replaces it atomically before reverse disposal', () => {
    const order: string[] = [];
    const runtime = createRuntimeSession({ createIdentityIssuer: identityFactory() });
    const initial = runtime.startInitialNavigation(frozenProjection('initial'));

    expect(initial.ok).toBe(true);
    if (!initial.ok) throw new Error('Expected initial navigation');
    expect(runtime.currentNavigation).toBe(initial.value);
    initial.value.onDispose('first', () => order.push('first'));
    initial.value.onDispose('second', () => {
      expect(runtime.currentNavigation).not.toBe(initial.value);
      order.push('second');
    });

    const replacement = runtime.replaceNavigation();

    expect(replacement.ok).toBe(true);
    if (!replacement.ok) throw new Error('Expected replacement navigation');
    expect(runtime.currentNavigation).toBe(replacement.value);
    expect(initial.value.disposed).toBe(true);
    expect(replacement.value.currentAuctionProjection).toBeUndefined();
    expect(order).toEqual(['second', 'first']);
    expect(runtime.snapshotInventoryForTest()).toMatchObject({
      currentNavigationGeneration: replacement.value.generation,
      disposedNavigations: 1,
      navigationCount: 1,
    });
  });

  it('makes late old-generation callbacks inert and allows the same DOM alias on a new route', () => {
    const runtime = createRuntimeSession({ createIdentityIssuer: identityFactory() });
    const initial = runtime.startInitialNavigation(frozenProjection('initial'));
    if (!initial.ok) throw new Error('Expected initial navigation');
    const mutation = vi.fn();
    const oldCallback = initial.value.capture(mutation);

    expect(initial.value.claimAlias('shared-dom-id')).toBe(true);
    expect(oldCallback('before')).toBe(true);
    const replacement = runtime.replaceNavigation();
    if (!replacement.ok) throw new Error('Expected replacement navigation');

    expect(replacement.value.claimAlias('shared-dom-id')).toBe(true);
    expect(oldCallback('late')).toBe(false);
    expect(mutation).toHaveBeenCalledExactlyOnceWith('before');
    expect(initial.value.snapshotInventoryForTest().aliases).toBe(0);
    expect(replacement.value.snapshotInventoryForTest().aliases).toBe(1);
  });

  it('does not publish the replacement while old-navigation disposers are running', () => {
    const runtime = createRuntimeSession({ createIdentityIssuer: identityFactory() });
    const initial = runtime.startInitialNavigation(frozenProjection('initial'));
    if (!initial.ok) throw new Error('Expected initial navigation');
    let disposerSawCurrent = true;
    let aliasMutation: boolean | undefined;
    initial.value.onDispose('reentrant-alias', () => {
      disposerSawCurrent = runtime.currentNavigation !== undefined;
      aliasMutation = runtime.currentNavigation?.claimAlias('must-not-cross-generation');
    });

    const replacement = runtime.replaceNavigation();

    expect(replacement).toMatchObject({ ok: true });
    if (!replacement.ok) throw new Error('Expected replacement navigation');
    expect(disposerSawCurrent).toBe(false);
    expect(aliasMutation).toBeUndefined();
    expect(runtime.currentNavigation).toBe(replacement.value);
    expect(replacement.value.disposed).toBe(false);
    expect(replacement.value.snapshotInventoryForTest().aliases).toBe(0);
  });

  it('blocks nested replacement from an old-navigation disposer', () => {
    const runtime = createRuntimeSession({ createIdentityIssuer: identityFactory() });
    const initial = runtime.startInitialNavigation(frozenProjection('initial'));
    if (!initial.ok) throw new Error('Expected initial navigation');
    let nestedReplacement: ReturnType<typeof runtime.replaceNavigation> | undefined;
    initial.value.onDispose('nested-replacement', () => {
      nestedReplacement = runtime.replaceNavigation();
    });

    const replacement = runtime.replaceNavigation();

    expect(nestedReplacement).toEqual({
      ok: false,
      reason: 'navigation_transition_in_progress',
    });
    expect(replacement).toMatchObject({ ok: true });
    if (!replacement.ok) throw new Error('Expected replacement navigation');
    expect(runtime.currentNavigation).toBe(replacement.value);
    expect(replacement.value.disposed).toBe(false);

    const successive = runtime.replaceNavigation();
    expect(successive).toMatchObject({ ok: true });
    if (!successive.ok) throw new Error('Expected successive replacement');
    expect(runtime.currentNavigation).toBe(successive.value);
    expect(successive.value.disposed).toBe(false);
  });

  it('publishes no replacement if runtime disposal occurs during old-navigation unwind', () => {
    const runtime = createRuntimeSession({ createIdentityIssuer: identityFactory() });
    const initial = runtime.startInitialNavigation(frozenProjection('initial'));
    if (!initial.ok) throw new Error('Expected initial navigation');
    initial.value.onDispose('runtime', () => runtime.dispose());

    expect(runtime.replaceNavigation()).toEqual({
      ok: false,
      reason: 'runtime_disposed',
    });
    expect(runtime.currentNavigation).toBeUndefined();
    expect(runtime.disposed).toBe(true);
  });

  it('publishes no initial navigation if identity setup disposes the runtime', () => {
    const issueIdentity = identityFactory();
    const runtime = createRuntimeSession({
      createIdentityIssuer: () => {
        runtime.dispose();
        return issueIdentity();
      },
    });

    expect(runtime.startInitialNavigation(frozenProjection('initial'))).toEqual({
      ok: false,
      reason: 'runtime_disposed',
    });
    expect(runtime.currentNavigation).toBeUndefined();
    expect(runtime.disposed).toBe(true);
  });

  it('blocks nested initial-navigation creation from identity setup', () => {
    const issueIdentity = identityFactory();
    let nested: ReturnType<ReturnType<typeof createRuntimeSession>['startInitialNavigation']>;
    let firstCall = true;
    const runtime = createRuntimeSession({
      createIdentityIssuer: () => {
        if (firstCall) {
          firstCall = false;
          nested = runtime.startInitialNavigation(frozenProjection('nested'));
        }
        return issueIdentity();
      },
    });

    const initial = runtime.startInitialNavigation(frozenProjection('initial'));

    expect(nested!).toEqual({
      ok: false,
      reason: 'navigation_transition_in_progress',
    });
    expect(initial).toMatchObject({ ok: true });
    if (!initial.ok) throw new Error('Expected initial navigation');
    expect(runtime.currentNavigation).toBe(initial.value);
    expect(initial.value.disposed).toBe(false);
  });

  it('cleans timers, listeners, and ports exactly once across double disposal', () => {
    const cleanup = {
      timer: vi.fn(),
      listener: vi.fn(),
      port: vi.fn(),
    };
    const runtime = createRuntimeSession({ createIdentityIssuer: identityFactory() });
    const navigation = runtime.startInitialNavigation(frozenProjection('initial'));
    if (!navigation.ok) throw new Error('Expected initial navigation');

    navigation.value.onDispose('timer', cleanup.timer);
    navigation.value.onDispose('listener', cleanup.listener);
    navigation.value.onDispose('port', cleanup.port);
    navigation.value.dispose();
    navigation.value.dispose();

    expect(cleanup.port).toHaveBeenCalledOnce();
    expect(cleanup.listener).toHaveBeenCalledOnce();
    expect(cleanup.timer).toHaveBeenCalledOnce();
    expect(navigation.value.snapshotInventoryForTest()).toMatchObject({
      disposed: true,
      activeDisposers: 0,
      disposedByKind: { listener: 1, port: 1, timer: 1 },
    });
  });

  it('clears an exactly current navigation after direct child disposal', () => {
    const runtime = createRuntimeSession({ createIdentityIssuer: identityFactory() });
    const initial = runtime.startInitialNavigation(frozenProjection('initial'));
    if (!initial.ok) throw new Error('Expected initial navigation');

    initial.value.dispose();

    expect(runtime.currentNavigation).toBeUndefined();
    expect(runtime.snapshotInventoryForTest()).toMatchObject({
      currentNavigationGeneration: undefined,
      disposedNavigations: 1,
      navigationCount: 0,
    });
    const replacement = runtime.replaceNavigation();
    expect(replacement).toMatchObject({ ok: true });
    if (!replacement.ok) throw new Error('Expected replacement navigation');
    expect(runtime.currentNavigation).toBe(replacement.value);
    expect(replacement.value.disposed).toBe(false);
  });

  it('blocks replacement before remaining direct-disposal callbacks can mutate a new route', () => {
    const runtime = createRuntimeSession({ createIdentityIssuer: identityFactory() });
    const initial = runtime.startInitialNavigation(frozenProjection('initial'));
    if (!initial.ok) throw new Error('Expected initial navigation');
    let replacement: ReturnType<typeof runtime.replaceNavigation> | undefined;
    let disposerSawCurrent = true;
    let aliasMutation: boolean | undefined;
    initial.value.onDispose('old-mutator', () => {
      disposerSawCurrent = runtime.currentNavigation !== undefined;
      aliasMutation = runtime.currentNavigation?.claimAlias('must-not-cross-generation');
    });
    initial.value.onDispose('replacement', () => {
      replacement = runtime.replaceNavigation();
    });

    initial.value.dispose();

    expect(replacement).toEqual({
      ok: false,
      reason: 'navigation_transition_in_progress',
    });
    expect(disposerSawCurrent).toBe(false);
    expect(aliasMutation).toBeUndefined();
    expect(runtime.currentNavigation).toBeUndefined();
    expect(runtime.snapshotInventoryForTest()).toMatchObject({
      currentNavigationGeneration: undefined,
      disposedNavigations: 1,
      navigationCount: 0,
    });

    const successive = runtime.replaceNavigation();
    expect(successive).toMatchObject({ ok: true });
    if (!successive.ok) throw new Error('Expected successive navigation');
    expect(runtime.currentNavigation).toBe(successive.value);
    expect(successive.value.snapshotInventoryForTest().aliases).toBe(0);
  });

  it('owns auction batches and render attempts in nested child scopes', () => {
    const order: string[] = [];
    const staleMutation = vi.fn();
    const runtime = createRuntimeSession({ createIdentityIssuer: identityFactory(7) });
    const navigation = runtime.startInitialNavigation(frozenProjection('initial'));
    if (!navigation.ok) throw new Error('Expected initial navigation');
    const batch = navigation.value.createAuctionBatch('batch-one');
    const overlappingBatch = navigation.value.createAuctionBatch('batch-two');

    expect(batch).toBeDefined();
    if (!batch) throw new Error('Expected auction batch');
    if (!overlappingBatch) throw new Error('Expected overlapping auction batch');
    const attempt = batch.createRenderAttempt('slot-one');
    const secondAttempt = batch.createRenderAttempt('slot-two');
    expect(attempt).toMatchObject({ ok: true });
    if (!attempt.ok) throw new Error('Expected render attempt');
    expect(secondAttempt).toMatchObject({ ok: true });
    if (!secondAttempt.ok) throw new Error('Expected second render attempt');
    expect(overlappingBatch.createRenderAttempt('slot-one')).toEqual({
      ok: false,
      reason: 'attempt_exists',
    });
    expect(attempt.value.id).toMatch(/^a1_[A-Za-z0-9_-]{22}$/);
    batch.onDispose('batch', () => order.push('batch'));
    batch.onDispose('late-callback', navigation.value.capture(staleMutation));
    attempt.value.onDispose('attempt-first', () => order.push('attempt-first'));
    attempt.value.onDispose('attempt-second', () => order.push('attempt-second'));
    expect(navigation.value.snapshotInventoryForTest()).toMatchObject({
      attempts: 2,
      batches: 2,
      retainedAttemptScopes: 2,
      retainedBatchScopes: 2,
    });

    secondAttempt.value.dispose();
    overlappingBatch.dispose();
    expect(navigation.value.snapshotInventoryForTest()).toMatchObject({
      attempts: 1,
      batches: 1,
      retainedAttemptScopes: 1,
      retainedBatchScopes: 1,
    });

    navigation.value.dispose();

    expect(staleMutation).not.toHaveBeenCalled();
    expect(order).toEqual(['attempt-second', 'attempt-first', 'batch']);
    expect(batch.disposed).toBe(true);
    expect(attempt.value.disposed).toBe(true);
    expect(navigation.value.snapshotInventoryForTest()).toMatchObject({
      attempts: 0,
      batches: 0,
    });
  });

  it('refuses identity failure before replacing or creating route work', () => {
    const firstIssuer = identityFactory();
    const createIdentityIssuer = vi
      .fn<NavigationIdentityIssuerFactory>()
      .mockImplementationOnce(firstIssuer)
      .mockReturnValue({ ok: false, reason: 'identity_generation_failed' });
    const runtime = createRuntimeSession({ createIdentityIssuer });
    const initial = runtime.startInitialNavigation(frozenProjection('initial'));
    if (!initial.ok) throw new Error('Expected initial navigation');
    const disposer = vi.fn();
    initial.value.onDispose('route', disposer);

    const replacement = runtime.replaceNavigation();

    expect(replacement).toEqual({ ok: false, reason: 'identity_generation_failed' });
    expect(runtime.currentNavigation).toBe(initial.value);
    expect(initial.value.disposed).toBe(false);
    expect(disposer).not.toHaveBeenCalled();
    expect(runtime.snapshotInventoryForTest()).toMatchObject({
      disposedNavigations: 0,
      navigationCount: 1,
    });
  });

  it('owns aliases, intents, targeting, batches, attempts, and one immutable projection', () => {
    const projection = frozenProjection('initial');
    const runtime = createRuntimeSession({ createIdentityIssuer: identityFactory() });
    const navigation = runtime.startInitialNavigation(projection);
    if (!navigation.ok) throw new Error('Expected initial navigation');

    expect(navigation.value.claimAlias('slot-alias')).toBe(true);
    expect(navigation.value.claimAlias('slot-alias')).toBe(false);
    expect(navigation.value.claimIntent('slot-one')).toBe(true);
    expect(navigation.value.claimTargeting('slot-one')).toBe(true);
    expect(navigation.value.currentAuctionProjection).toBe(projection);
    expect(Object.isFrozen(navigation.value.currentAuctionProjection)).toBe(true);
    expect(navigation.value.snapshotInventoryForTest()).toMatchObject({
      aliases: 1,
      attempts: 0,
      batches: 0,
      intents: 1,
      targetingOwners: 1,
    });
  });

  it('owns injected interfaces and runtime disposers without exposing mutable inventory', () => {
    const order: string[] = [];
    const interfaces = Object.freeze({ messaging: Object.freeze({ active: true }) });
    const runtime = createRuntimeSession({
      createIdentityIssuer: identityFactory(),
      interfaces,
    });
    runtime.onDispose('adapter', () => order.push('adapter'));
    runtime.onDispose('service', () => order.push('service'));

    expect(runtime.interfaces).toBe(interfaces);
    expect(Object.isFrozen(runtime.interfaces)).toBe(true);
    runtime.dispose();
    runtime.dispose();

    expect(order).toEqual(['service', 'adapter']);
    const inventory = runtime.snapshotInventoryForTest();
    expect(Object.isFrozen(inventory)).toBe(true);
    expect(inventory).toMatchObject({ disposed: true, activeDisposers: 0 });
  });
});
