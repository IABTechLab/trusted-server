import { describe, expect, it, vi } from 'vitest';

import {
  createGoogleTagManagerIntegrationRegistration,
  createGoogleTagManagerRuntime,
} from '../../../src/integrations/google_tag_manager/module';
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

describe('transactional Google Tag Manager integration module', () => {
  it('activates both guards before publication and releases them in reverse order', async () => {
    const order: string[] = [];
    const runtime = createGoogleTagManagerRuntime({
      installBeaconGuard: () => order.push('beacon:install'),
      installScriptGuard: () => order.push('script:install'),
      resetBeaconGuard: () => order.push('beacon:reset'),
      resetScriptGuard: () => order.push('script:reset'),
      started: () => order.push('gtm:start'),
    });
    const registry = createIntegrationRegistry({
      manifest: {
        version: 1,
        releaseId: RELEASE_ID,
        criticalSrc: CRITICAL_SRC,
        integrations: [{ id: 'google_tag_manager', phase: 'critical' }],
      },
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['google_tag_manager']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: undefined,
        interfaces: Object.freeze({ google_tag_manager: runtime }),
      }),
    });
    registry.register(createGoogleTagManagerIntegrationRegistration(RELEASE_ID));

    expect(order).toEqual([]);
    const result = await registry.install(callbacks(order));

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'core',
      'script:install',
      'beacon:install',
      'publish',
      'gtm:start',
      'drain',
    ]);
    if (result.state === 'kernel') result.dispose();
    expect(order.slice(-2)).toEqual(['beacon:reset', 'script:reset']);
  });

  it('rolls back the script guard when beacon activation throws', () => {
    const resetBeaconGuard = vi.fn();
    const resetScriptGuard = vi.fn();
    const runtime = createGoogleTagManagerRuntime({
      installBeaconGuard: () => {
        throw new Error('fictional beacon failure');
      },
      installScriptGuard: vi.fn(),
      resetBeaconGuard,
      resetScriptGuard,
      started: vi.fn(),
    });

    expect(() => runtime.activate(undefined)).toThrowError('fictional beacon failure');
    expect(resetBeaconGuard).toHaveBeenCalledOnce();
    expect(resetScriptGuard).toHaveBeenCalledOnce();
  });

  it('rolls back an attempted script guard installation that throws', () => {
    const resetBeaconGuard = vi.fn();
    const resetScriptGuard = vi.fn();
    const runtime = createGoogleTagManagerRuntime({
      installBeaconGuard: vi.fn(),
      installScriptGuard: () => {
        throw new Error('fictional script failure');
      },
      resetBeaconGuard,
      resetScriptGuard,
      started: vi.fn(),
    });

    expect(() => runtime.activate(undefined)).toThrowError('fictional script failure');
    expect(resetBeaconGuard).not.toHaveBeenCalled();
    expect(resetScriptGuard).toHaveBeenCalledOnce();
  });
});
