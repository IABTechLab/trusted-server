import { describe, expect, it, vi } from 'vitest';

import {
  createIntegrationRegistry,
  type IntegrationInstallCallbacks,
} from '../../src/kernel/integration_registry';
import { createLifecycleIntegrationRegistration } from '../../src/kernel/lifecycle_module';

const RELEASE_ID = 'a'.repeat(64);
const CRITICAL_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;
const TEST_INTEGRATION_ID = 'datadome';

function callbacks(order: string[]): IntegrationInstallCallbacks {
  return {
    activateCore: () => order.push('core'),
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  };
}

function registry(config: unknown, runtime: unknown) {
  return createIntegrationRegistry({
    manifest: {
      version: 1,
      releaseId: RELEASE_ID,
      criticalSrc: CRITICAL_SRC,
      integrations: [{ id: TEST_INTEGRATION_ID, phase: 'critical' }],
    },
    releaseId: RELEASE_ID,
    knownIntegrationIds: Object.freeze([TEST_INTEGRATION_ID]),
    startedAtMs: 0,
    now: () => 0,
    getBindings: () => ({
      config,
      interfaces: Object.freeze({ [TEST_INTEGRATION_ID]: runtime }),
    }),
  });
}

describe('shared integration lifecycle module', () => {
  it('prepares inertly, activates reversibly, and starts only after publication', async () => {
    const order: string[] = [];
    const config = Object.freeze({ nested: Object.freeze({ enabled: true }) });
    const release = vi.fn(() => order.push('release'));
    const activate = vi.fn((received: unknown) => {
      expect(received).toBe(config);
      order.push('activate');
      return release;
    });
    const start = vi.fn((received: unknown) => {
      expect(received).toBe(config);
      order.push('start');
    });
    const runtime = Object.freeze({ activate, start });
    const owner = registry(config, runtime);
    owner.register(createLifecycleIntegrationRegistration(TEST_INTEGRATION_ID, RELEASE_ID));

    const result = await owner.install(callbacks(order));

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual(['core', 'activate', 'publish', 'start', 'drain']);
    if (result.state === 'kernel') result.dispose();
    expect(release).toHaveBeenCalledOnce();
  });

  it.each([
    ['mutable root', { enabled: true }],
    ['mutable nested value', Object.freeze({ nested: { enabled: true } })],
    ['accessor', Object.freeze(Object.defineProperty({}, 'enabled', { get: () => true }))],
    ['function', Object.freeze(() => undefined)],
  ])('rejects %s configuration before activation', async (_name, config) => {
    const activate = vi.fn(() => vi.fn());
    const owner = registry(config, Object.freeze({ activate, start: vi.fn() }));
    owner.register(createLifecycleIntegrationRegistration(TEST_INTEGRATION_ID, RELEASE_ID));

    await expect(owner.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(activate).not.toHaveBeenCalled();
  });

  it('rejects extra runtime authority and unwinds activation when startup peers fail', async () => {
    const activate = vi.fn(() => vi.fn());
    const owner = registry(
      Object.freeze({}),
      Object.freeze({ activate, start: vi.fn(), publish: vi.fn() })
    );
    owner.register(createLifecycleIntegrationRegistration(TEST_INTEGRATION_ID, RELEASE_ID));

    await expect(owner.install(callbacks([]))).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(activate).not.toHaveBeenCalled();
  });

  it.each([
    { selected: true, state: true, expected: 'kernel' },
    { selected: true, state: false, expected: 'fallback' },
    { selected: false, state: false, expected: 'kernel' },
  ] as const)(
    'validates selected first-display parser state before lifecycle activation ($selected, $state)',
    async ({ selected, state, expected }) => {
      const activate = vi.fn(() => vi.fn());
      const owner = registry(Object.freeze({}), Object.freeze({ activate, start: vi.fn() }));
      owner.register(
        createLifecycleIntegrationRegistration(TEST_INTEGRATION_ID, RELEASE_ID, {
          firstDisplaySliceId: 'datadome_initial',
          validateFirstDisplayState: (candidate) =>
            candidate.values.find(([key]) => key === 'route_guard')?.[1] === 'datadome',
        })
      );
      const adoption = Object.freeze({
        version: 1 as const,
        adoptInitialDisplay: true as const,
        handoff: Object.freeze({
          slices: Object.freeze(
            selected ? ['first_display', 'datadome_initial'] : ['first_display']
          ),
          parserState: Object.freeze(
            state
              ? [
                  Object.freeze({
                    sliceId: 'datadome_initial',
                    observations: Object.freeze(['route_guard']),
                    values: Object.freeze([Object.freeze(['route_guard', 'datadome'] as const)]),
                  }),
                ]
              : []
          ),
        }),
        identities: Object.freeze([]),
      });

      const result = await owner.install({
        ...callbacks([]),
        coordinateTakeover: (prepared) => {
          prepared.activate(adoption);
          prepared.commit();
        },
      });

      expect(result.state).toBe(expected);
      expect(activate).toHaveBeenCalledTimes(expected === 'kernel' ? 1 : 0);
    }
  );
});
