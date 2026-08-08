import { describe, expect, it, vi } from 'vitest';

import { createCreativeIntegrationRegistration } from '../../../src/integrations/creative/module';
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

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

function registration(
  id: string,
  prepare: IntegrationRegistration['prepare']
): IntegrationRegistration {
  return Object.freeze({ id, release: RELEASE_ID, prepare });
}

describe('transactional creative integration module', () => {
  it('prepares inertly, activates reversible guards, and scans only after commit', async () => {
    const config = Object.freeze({
      version: 1,
      enabled: true,
      clickGuard: true,
      renderGuard: true,
    });
    const order: string[] = [];
    const release = vi.fn(() => order.push('release'));
    const activate = vi.fn((received: unknown) => {
      order.push('creative:activate');
      expect(received).toBe(config);
      return release;
    });
    const start = vi.fn(() => order.push('creative:scan'));
    let finishPreparation: (() => void) | undefined;
    const preparationGate = new Promise<void>((resolve) => {
      finishPreparation = resolve;
    });
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative', 'gate']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative', 'gate']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({ creative: Object.freeze({ activate, start }) }),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));
    registry.register(
      registration('gate', async () => {
        order.push('gate:prepare');
        await preparationGate;
        return Object.freeze({ activate: () => order.push('gate:activate') });
      })
    );

    const installing = registry.install(callbacks(order));
    await vi.waitFor(() => expect(order).toEqual(['gate:prepare']));
    expect(activate).not.toHaveBeenCalled();
    expect(start).not.toHaveBeenCalled();

    finishPreparation?.();
    const result = await installing;

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'gate:prepare',
      'core',
      'creative:activate',
      'gate:activate',
      'publish',
      'creative:scan',
      'drain',
    ]);
    if (result.state === 'kernel') {
      result.dispose();
      result.dispose();
    }
    expect(release).toHaveBeenCalledTimes(1);
  });

  it.each([
    Object.freeze({ version: 1, enabled: false, clickGuard: true, renderGuard: true }),
    Object.freeze({ version: 1, enabled: true, clickGuard: false, renderGuard: false }),
  ])('performs no runtime work for an inactive creative boot %#', async (config) => {
    const activate = vi.fn();
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({ creative: Object.freeze({ activate, start }) }),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({ state: 'kernel' });
    expect(activate).not.toHaveBeenCalled();
    expect(start).not.toHaveBeenCalled();
  });

  it('unwinds creative activation before a later module failure', async () => {
    const release = vi.fn();
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative', 'broken']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative', 'broken']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({
          version: 1,
          enabled: true,
          clickGuard: true,
          renderGuard: false,
        }),
        interfaces: Object.freeze({
          creative: Object.freeze({ activate: () => release, start }),
        }),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));
    registry.register(
      registration('broken', () => ({
        activate: () => {
          throw new Error('fictional creative peer failure');
        },
      }))
    );

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(release).toHaveBeenCalledTimes(1);
    expect(start).not.toHaveBeenCalled();
  });

  it.each([
    ['missing field', Object.freeze({ version: 1, enabled: true, clickGuard: true })],
    [
      'unknown field',
      Object.freeze({
        version: 1,
        enabled: true,
        clickGuard: true,
        renderGuard: false,
        extra: true,
      }),
    ],
    [
      'accessor',
      Object.freeze(
        Object.defineProperty({ version: 1, enabled: true, clickGuard: true }, 'renderGuard', {
          enumerable: true,
          get: () => false,
        })
      ),
    ],
    [
      'non-plain object',
      Object.freeze(
        Object.assign(Object.create({ inherited: true }) as object, {
          version: 1,
          enabled: true,
          clickGuard: true,
          renderGuard: false,
        })
      ),
    ],
    ['mutable object', { version: 1, enabled: true, clickGuard: true, renderGuard: false }],
  ])('rejects %s configuration during inert preparation', async (_caseName, config) => {
    const activate = vi.fn();
    const start = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({ creative: Object.freeze({ activate, start }) }),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(activate).not.toHaveBeenCalled();
    expect(start).not.toHaveBeenCalled();
  });

  it('fails preparation without effects when composition omits the creative boundary', async () => {
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({
          version: 1,
          enabled: true,
          clickGuard: true,
          renderGuard: false,
        }),
        interfaces: Object.freeze({}),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
  });

  it('isolates a post-commit scan failure to the creative module', async () => {
    const runtimeFailures: unknown[] = [];
    const start = vi.fn(() => {
      throw new Error('fictional creative scan failure');
    });
    const registry = createIntegrationRegistry({
      manifest: manifest(['creative']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['creative']),
      startedAtMs: 0,
      now: () => 0,
      onRuntimeFailure: (failure) => runtimeFailures.push(failure),
      getBindings: () => ({
        config: Object.freeze({
          version: 1,
          enabled: true,
          clickGuard: true,
          renderGuard: false,
        }),
        interfaces: Object.freeze({
          creative: Object.freeze({ activate: () => vi.fn(), start }),
        }),
      }),
    });
    registry.register(createCreativeIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'kernel',
      runtimeFailures: [{ id: 'creative', phase: 'after_commit' }],
    });
    expect(runtimeFailures).toEqual([{ id: 'creative', phase: 'after_commit' }]);
  });
});
