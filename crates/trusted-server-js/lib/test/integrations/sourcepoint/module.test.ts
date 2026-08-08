import { describe, expect, it, vi } from 'vitest';

import {
  createSourcepointIntegrationRegistration,
  createSourcepointRuntime,
} from '../../../src/integrations/sourcepoint/module';
import { createIntegrationRegistry } from '../../../src/kernel/integration_registry';

const RELEASE_ID = 'a'.repeat(64);

describe('transactional Sourcepoint integration module', () => {
  it.each([true, false])(
    'owns the optional SDK guard and consent mirror when rewriteSdk=%s',
    (rewriteSdk) => {
      const order: string[] = [];
      const runtime = createSourcepointRuntime({
        initializeConsentMirror: () => order.push('start:consent'),
        installGuard: () => order.push('activate:guard'),
        resetConsentMirror: () => order.push('dispose:consent'),
        resetGuard: () => order.push('dispose:guard'),
      });
      const config = Object.freeze({ rewriteSdk });

      const release = runtime.activate(config);
      runtime.start(config);
      release();
      release();

      expect(order).toEqual(
        rewriteSdk
          ? ['activate:guard', 'start:consent', 'dispose:consent', 'dispose:guard']
          : ['start:consent', 'dispose:consent']
      );
    }
  );

  it.each([
    ['missing', undefined],
    ['mutable', { rewriteSdk: true }],
    ['wrong type', Object.freeze({ rewriteSdk: 'yes' })],
    ['extra', Object.freeze({ rewriteSdk: true, legacy: true })],
  ])('rejects %s boot config before activation', async (_name, config) => {
    const activate = vi.fn(() => vi.fn());
    const registry = createIntegrationRegistry({
      manifest: {
        version: 1,
        releaseId: RELEASE_ID,
        integrations: [{ id: 'sourcepoint', required: true }],
      },
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['sourcepoint']),
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config,
        interfaces: Object.freeze({
          sourcepoint: Object.freeze({ activate, start: vi.fn() }),
        }),
      }),
    });
    registry.register(createSourcepointIntegrationRegistration(RELEASE_ID));

    await expect(
      registry.install({ activateCore: vi.fn(), publish: vi.fn(), drainPreload: vi.fn() })
    ).resolves.toMatchObject({ state: 'fallback', reason: 'bundle_partial' });
    expect(activate).not.toHaveBeenCalled();
  });
});
