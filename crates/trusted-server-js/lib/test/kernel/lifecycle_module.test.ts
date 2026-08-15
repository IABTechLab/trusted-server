import { describe, expect, it, vi } from 'vitest';

import {
  createIntegrationRegistry,
  type IntegrationInstallCallbacks,
} from '../../src/kernel/integration_registry';
import { createLifecycleIntegrationRegistration } from '../../src/kernel/lifecycle_module';

const RELEASE_ID = 'a'.repeat(64);
const RUNTIME_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;
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
      firstDisplay: null,
      runtimeSrc: RUNTIME_SRC,
      integrations: [{ id: TEST_INTEGRATION_ID, phase: 'takeover' }],
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
        handoff: {
          version: 1,
          releaseId: RELEASE_ID,
          generation: 1,
          projectionDigest: 'b'.repeat(64),
          slices: Object.freeze(
            selected ? ['first_display', 'datadome_initial'] : ['first_display']
          ),
          slots: [
            {
              id: 'slot-1',
              aliases: [],
              domId: 'slot-1',
              gamPath: '/123/slot-1',
              formats: [[300, 250]],
              owner: 'trusted_server',
              outcome: 'failed',
              targeting: [],
              committedArtifact: 'none',
              gptToken: null,
            },
          ],
          attempts: [
            {
              id: 'a1_AAECAwQFBgcAAAAAAAAAAQ',
              slotId: 'slot-1',
              ordinal: 1,
              state: 'failed',
              reason: 'internal_error',
            },
          ],
          tombstones: [],
          artifacts: [],
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
          gptDiagnostics: { facts: [], overflowCount: 0, dropCount: 0 },
          timing: { bidsScriptMs: 1, firstDisplayMs: null, terminalMs: 2, paintMs: 3 },
          highWater: {
            navigationAttemptPrefix: 'AAECAwQFBgc',
            nextNavigationAttemptOrdinal: 2,
            nextAttemptOrdinal: 2,
            nextSlotRegistrationOrdinal: 2,
            reservationClockEpochMs: 0,
            nextReservationOrdinal: 1,
            nextTicketOrdinal: 1,
          },
          cycles: [],
          trace: {
            nextSequence: 1,
            nextGlobalSlotOrdinal: 1,
            slots: [{ slotId: 'slot-1', impressions: 0, bindings: [] }],
          },
          mutationRevision: 0,
        },
        identities: Object.freeze([]),
      });

      const result = await owner.install({
        ...callbacks([]),
        coordinateTakeover: (prepared) => {
          const slices = selected ? ['first_display', 'datadome_initial'] : ['first_display'];
          const handoff = prepared.validateHandoff(adoption.handoff, {
            version: 1,
            releaseId: RELEASE_ID,
            generation: 1,
            projectionDigest: 'b'.repeat(64),
            slices,
            slotCount: 1,
            outcomeCount: 1,
            capabilities: [],
            objectKinds: [],
          });
          if (!handoff) throw new Error('should validate lifecycle handoff');
          prepared.activate(Object.freeze({ ...adoption, handoff }));
          prepared.commit();
        },
      });

      expect(result.state).toBe(expected);
      expect(activate).toHaveBeenCalledTimes(expected === 'kernel' ? 1 : 0);
    }
  );
});
