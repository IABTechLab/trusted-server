import { describe, expect, it, vi } from 'vitest';

import {
  createDataDomeIntegrationRegistration,
  createDataDomeRuntime,
} from '../../../src/integrations/datadome/module';
import {
  createIntegrationRegistry,
  type IntegrationInstallCallbacks,
} from '../../../src/kernel/integration_registry';

const RELEASE_ID = 'a'.repeat(64);
const CRITICAL_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

describe('transactional DataDome integration module', () => {
  it('prepares inertly, activates before publication, and releases exactly once', async () => {
    const order: string[] = [];
    const release = vi.fn(() => order.push('release'));
    const runtime = Object.freeze({
      activate: vi.fn(() => {
        order.push('datadome:activate');
        return release;
      }),
      start: vi.fn(() => order.push('datadome:start')),
    });
    const registry = createIntegrationRegistry({
      manifest: {
        version: 1,
        releaseId: RELEASE_ID,
        criticalSrc: CRITICAL_SRC,
        integrations: [{ id: 'datadome', phase: 'critical' }],
      },
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['datadome']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: undefined,
        interfaces: Object.freeze({ datadome: runtime }),
      }),
    });
    registry.register(createDataDomeIntegrationRegistration(RELEASE_ID));

    expect(runtime.activate).not.toHaveBeenCalled();
    expect(runtime.start).not.toHaveBeenCalled();
    const result = await registry.install(callbacks(order));

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual(['core', 'datadome:activate', 'publish', 'datadome:start', 'drain']);
    if (result.state === 'kernel') {
      result.dispose();
      result.dispose();
    }
    expect(release).toHaveBeenCalledOnce();
  });

  it.each([null, Object.freeze({}), false])('rejects non-absent config %j', async (config) => {
    const activate = vi.fn(() => vi.fn());
    const registry = createIntegrationRegistry({
      manifest: {
        version: 1,
        releaseId: RELEASE_ID,
        criticalSrc: CRITICAL_SRC,
        integrations: [{ id: 'datadome', phase: 'critical' }],
      },
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['datadome']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({
          datadome: Object.freeze({ activate, start: vi.fn() }),
        }),
      }),
    });
    registry.register(createDataDomeIntegrationRegistration(RELEASE_ID));

    await expect(registry.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(activate).not.toHaveBeenCalled();
  });

  it('owns and reverses the concrete DataDome guard', () => {
    const order: string[] = [];
    const runtime = createDataDomeRuntime({
      installGuard: () => order.push('install'),
      resetGuard: () => order.push('reset'),
      started: () => order.push('started'),
    });

    const release = runtime.activate(undefined);
    runtime.start(undefined);
    release();
    release();

    expect(order).toEqual(['install', 'started', 'reset']);
  });

  it('rolls back an attempted guard installation that throws', () => {
    const resetGuard = vi.fn();
    const runtime = createDataDomeRuntime({
      installGuard: () => {
        throw new Error('fictional guard failure');
      },
      resetGuard,
      started: vi.fn(),
    });

    expect(() => runtime.activate(undefined)).toThrowError('fictional guard failure');
    expect(resetGuard).toHaveBeenCalledOnce();
  });
});
