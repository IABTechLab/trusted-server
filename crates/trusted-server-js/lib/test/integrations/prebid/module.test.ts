import { describe, expect, it, vi } from 'vitest';

import { createPrebidIntegrationRegistration } from '../../../src/integrations/prebid/module';
import {
  createIntegrationRegistry,
  type IntegrationInstallCallbacks,
  type IntegrationRegistration,
} from '../../../src/kernel/integration_registry';

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
