import { describe, expect, it, vi } from 'vitest';

import { createDataDomeIntegrationRegistration } from '../../src/integrations/datadome/module';
import { createDidomiIntegrationRegistration } from '../../src/integrations/didomi/module';
import { createGoogleTagManagerIntegrationRegistration } from '../../src/integrations/google_tag_manager/module';
import { createLockrIntegrationRegistration } from '../../src/integrations/lockr/module';
import { createOsanoIntegrationRegistration } from '../../src/integrations/osano/module';
import { createPermutiveIntegrationRegistration } from '../../src/integrations/permutive/module';
import { createSourcepointIntegrationRegistration } from '../../src/integrations/sourcepoint/module';
import { createTestlightIntegrationRegistration } from '../../src/integrations/testlight/module';
import {
  createIntegrationRegistry,
  type IntegrationRegistration,
} from '../../src/kernel/integration_registry';

const RELEASE_ID = 'a'.repeat(64);
const registrations: ReadonlyArray<
  readonly [string, (release: string) => IntegrationRegistration]
> = Object.freeze([
  ['datadome', createDataDomeIntegrationRegistration] as const,
  ['didomi', createDidomiIntegrationRegistration] as const,
  ['google_tag_manager', createGoogleTagManagerIntegrationRegistration] as const,
  ['lockr', createLockrIntegrationRegistration] as const,
  ['osano', createOsanoIntegrationRegistration] as const,
  ['permutive', createPermutiveIntegrationRegistration] as const,
  ['sourcepoint', createSourcepointIntegrationRegistration] as const,
  ['testlight', createTestlightIntegrationRegistration] as const,
]);
const configFor = (id: string): unknown => {
  if (id === 'didomi') return Object.freeze({ proxyPath: '/integrations/didomi/sdk' });
  if (id === 'sourcepoint') return Object.freeze({ rewriteSdk: true });
  return undefined;
};

describe('remaining integration lifecycle modules', () => {
  it('activates a maximal manifest once in order and disposes it in exact reverse order', async () => {
    const order: string[] = [];
    const ids = Object.freeze(registrations.map(([id]) => id));
    const interfaces = Object.freeze(
      Object.fromEntries(
        ids.map((id) => [
          id,
          Object.freeze({
            activate: (config: unknown) => {
              expect(config).toEqual(configFor(id));
              order.push(`activate:${id}`);
              return () => order.push(`dispose:${id}`);
            },
            start: () => order.push(`start:${id}`),
          }),
        ])
      )
    );
    const registry = createIntegrationRegistry({
      manifest: {
        version: 1,
        releaseId: RELEASE_ID,
        integrations: ids.map((id) => ({ id, required: true })),
      },
      releaseId: RELEASE_ID,
      knownIntegrationIds: ids,
      startedAtMs: 0,
      now: () => 0,
      getBindings: (id) => ({ config: configFor(id), interfaces }),
    });
    for (const [, createRegistration] of registrations) {
      expect(registry.register(createRegistration(RELEASE_ID))).toBe(true);
    }

    const result = await registry.install({
      activateCore: () => order.push('core'),
      publish: () => order.push('publish'),
      drainPreload: () => order.push('drain'),
    });

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'core',
      ...ids.map((id) => `activate:${id}`),
      'publish',
      ...ids.map((id) => `start:${id}`),
      'drain',
    ]);
    if (result.state === 'kernel') result.dispose();
    expect(order.slice(-ids.length)).toEqual([...ids].reverse().map((id) => `dispose:${id}`));
  });

  it.each(registrations)(
    '%s runs alone without cross-integration authority',
    async (id, create) => {
      const activate = vi.fn(() => vi.fn());
      const start = vi.fn();
      const registry = createIntegrationRegistry({
        manifest: {
          version: 1,
          releaseId: RELEASE_ID,
          integrations: [{ id, required: true }],
        },
        releaseId: RELEASE_ID,
        knownIntegrationIds: Object.freeze([id]),
        startedAtMs: 0,
        now: () => 0,
        getBindings: () => ({
          config: configFor(id),
          interfaces: Object.freeze({ [id]: Object.freeze({ activate, start }) }),
        }),
      });
      registry.register(create(RELEASE_ID));

      await expect(
        registry.install({ activateCore: vi.fn(), publish: vi.fn(), drainPreload: vi.fn() })
      ).resolves.toMatchObject({ state: 'kernel' });
      expect(activate).toHaveBeenCalledOnce();
      expect(start).toHaveBeenCalledOnce();
    }
  );
});
