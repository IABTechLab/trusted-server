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
import { RELEASE_CATALOG } from '../../src/kernel/release_catalog';

const RELEASE_ID = 'a'.repeat(64);
const CRITICAL_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;
const registrations: ReadonlyArray<
  readonly [string, (release: string) => IntegrationRegistration]
> = Object.freeze([
  ['datadome', createDataDomeIntegrationRegistration] as const,
  ['didomi', createDidomiIntegrationRegistration] as const,
  ['google_tag_manager', createGoogleTagManagerIntegrationRegistration] as const,
  ['lockr', createLockrIntegrationRegistration] as const,
  ['osano_consent', createOsanoIntegrationRegistration] as const,
  ['permutive_context', createPermutiveIntegrationRegistration] as const,
  ['sourcepoint_consent', createSourcepointIntegrationRegistration] as const,
  ['testlight', createTestlightIntegrationRegistration] as const,
]);
const configFor = (id: string): unknown => {
  if (id === 'didomi') return Object.freeze({ proxyPath: '/integrations/didomi/sdk' });
  if (id === 'sourcepoint_consent') return Object.freeze({ rewriteSdk: true });
  return undefined;
};

function catalogFor(ids: readonly string[]) {
  return Object.freeze(
    ids.map((id) => {
      const entry = RELEASE_CATALOG.find((candidate) => candidate.id === id);
      if (!entry) throw new TypeError(`Missing release catalog row: ${id}`);
      return Object.freeze({
        id: entry.id,
        phase: entry.phase,
        trigger: entry.trigger,
        consumes: Object.freeze([...entry.consumes]),
        provides: Object.freeze([...entry.provides]),
      });
    })
  );
}

function runtimeCapability() {
  return Object.freeze({
    document,
    enqueue: (callback: () => void) => {
      callback();
      return true;
    },
    registerAuctionContext: () => () => undefined,
  });
}

describe('remaining integration lifecycle modules', () => {
  it('activates the provider-owned maximal lifecycle set without foreign runtime authority', async () => {
    const order: string[] = [];
    const ids = Object.freeze(registrations.map(([id]) => id));
    const foreignActivations = new Map<string, ReturnType<typeof vi.fn>>();
    const foreignStarts = new Map<string, ReturnType<typeof vi.fn>>();
    const interfaces = Object.freeze(
      Object.fromEntries(
        ids.map((id) => {
          const activate = vi.fn(() => vi.fn());
          const start = vi.fn();
          foreignActivations.set(id, activate);
          foreignStarts.set(id, start);
          return [id, Object.freeze({ activate, start })];
        })
      )
    );
    const registry = createIntegrationRegistry({
      manifest: {
        version: 1,
        releaseId: RELEASE_ID,
        criticalSrc: CRITICAL_SRC,
        integrations: ids.map((id) => ({ id, phase: 'critical' as const })),
      },
      releaseId: RELEASE_ID,
      knownIntegrationIds: ids,
      catalog: catalogFor(ids),
      startedAtMs: 0,
      now: () => 0,
      runtimeCapability: runtimeCapability(),
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
    expect(order).toEqual(['core', 'publish', 'drain']);
    for (const id of ids) {
      expect(foreignActivations.get(id)).not.toHaveBeenCalled();
      expect(foreignStarts.get(id)).not.toHaveBeenCalled();
    }
    if (result.state === 'kernel') result.dispose();
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
          criticalSrc: CRITICAL_SRC,
          integrations: [{ id, phase: 'critical' }],
        },
        releaseId: RELEASE_ID,
        knownIntegrationIds: Object.freeze([id]),
        catalog: catalogFor([id]),
        startedAtMs: 0,
        now: () => 0,
        runtimeCapability: runtimeCapability(),
        getBindings: () => ({
          config: configFor(id),
          interfaces: Object.freeze({ [id]: Object.freeze({ activate, start }) }),
        }),
      });
      registry.register(create(RELEASE_ID));

      await expect(
        registry.install({ activateCore: vi.fn(), publish: vi.fn(), drainPreload: vi.fn() })
      ).resolves.toMatchObject({ state: 'kernel' });
      expect(activate).not.toHaveBeenCalled();
      expect(start).not.toHaveBeenCalled();
      registry.dispose();
    }
  );
});
