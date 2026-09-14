import { beforeEach, describe, expect, it, vi } from 'vitest';

const ownedGuards = vi.hoisted(() => ({
  installBeacon: vi.fn(),
  installScript: vi.fn(),
  resetBeacon: vi.fn(),
  resetScript: vi.fn(),
}));

vi.mock('../../../src/integrations/google_tag_manager/script_guard', () => ({
  installGtmBeaconGuard: ownedGuards.installBeacon,
  installGtmGuard: ownedGuards.installScript,
  resetBeaconGuardState: ownedGuards.resetBeacon,
  resetGuardState: ownedGuards.resetScript,
}));

import {
  createGoogleTagManagerIntegrationRegistration,
  createGoogleTagManagerRuntime,
} from '../../../src/integrations/google_tag_manager/module';
import {
  createIntegrationRegistry,
  type IntegrationInstallCallbacks,
} from '../../../src/kernel/integration_registry';

const RELEASE_ID = 'a'.repeat(64);
const RUNTIME_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

describe('transactional Google Tag Manager integration module', () => {
  beforeEach(() => {
    ownedGuards.installBeacon.mockReset();
    ownedGuards.installScript.mockReset();
    ownedGuards.resetBeacon.mockReset();
    ownedGuards.resetScript.mockReset();
  });

  it('activates both guards before publication and releases them in reverse order', async () => {
    const order: string[] = [];
    ownedGuards.installBeacon.mockImplementation(() => order.push('beacon:install'));
    ownedGuards.installScript.mockImplementation(() => order.push('script:install'));
    ownedGuards.resetBeacon.mockImplementation(() => order.push('beacon:reset'));
    ownedGuards.resetScript.mockImplementation(() => order.push('script:reset'));
    const registry = createIntegrationRegistry({
      manifest: {
        version: 1,
        releaseId: RELEASE_ID,
        firstDisplay: null,
        runtimeSrc: RUNTIME_SRC,
        integrations: [{ id: 'google_tag_manager', phase: 'takeover' }],
      },
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['google_tag_manager']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({}),
        interfaces: Object.freeze({}),
      }),
    });
    registry.register(createGoogleTagManagerIntegrationRegistration(RELEASE_ID));

    expect(order).toEqual([]);
    const result = await registry.install(callbacks(order));

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual(['core', 'script:install', 'beacon:install', 'publish', 'drain']);
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
