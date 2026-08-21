import { afterEach, describe, expect, it, vi } from 'vitest';

import type { BootManifestV1 } from '../../src/core/types';
import { EMBEDDED_MAX_MANIFEST_MODULES } from '../../src/kernel/contracts/release_capacity';
import {
  createIntegrationRegistry as createIntegrationRegistryOwner,
  snapshotIntegrationRegistration,
  type IntegrationPrepareContext,
  type IntegrationRegistryOptions,
  type TakeoverIntegrationRegistration,
} from '../../src/kernel/integration_registry';
import { snapshotPersistentFirstDisplayAdoptionV1 } from '../../src/shared/takeover';

const RELEASE_ID = 'a'.repeat(64);
const OTHER_RELEASE_ID = 'b'.repeat(64);

type TestRegistryOptions = Omit<IntegrationRegistryOptions, 'knownIntegrationIds'> & {
  readonly knownIntegrationIds?: readonly string[];
};

function manifestIds(candidate: unknown): readonly string[] {
  if (typeof candidate !== 'object' || candidate === null) return Object.freeze([]);
  const integrations = (candidate as { integrations?: unknown }).integrations;
  if (!Array.isArray(integrations)) return Object.freeze([]);

  const ids: string[] = [];
  for (let index = 0; index < integrations.length; index += 1) {
    const entry = integrations[index] as { id?: unknown } | undefined;
    if (typeof entry?.id === 'string') ids.push(entry.id);
  }
  return Object.freeze([...new Set(ids)]);
}

function createIntegrationRegistry(options: TestRegistryOptions) {
  const knownIntegrationIds = options.knownIntegrationIds ?? manifestIds(options.manifest);
  return createIntegrationRegistryOwner({
    ...options,
    knownIntegrationIds,
    catalog: Object.freeze(
      knownIntegrationIds.map((id) =>
        Object.freeze({
          id,
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze([]),
          provides: Object.freeze([]),
        })
      )
    ),
  });
}

function manifest(ids: readonly string[]): BootManifestV1 {
  return {
    version: 1,
    releaseId: RELEASE_ID,
    firstDisplay: null,
    runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
    integrations: ids.map((id) => ({ id, phase: 'takeover' as const })),
  };
}

function registration(
  id: string,
  hooks: Partial<TakeoverIntegrationRegistration> = {}
): TakeoverIntegrationRegistration {
  const prepare = hooks.prepare ?? (() => Object.freeze({ activate: () => undefined }));
  return {
    abi: 1,
    id,
    phase: 'takeover',
    releaseId: RELEASE_ID,
    prepareSync: hooks.prepareSync ?? (() => Object.freeze({ activate: () => undefined })),
    prepare,
    ...hooks,
  };
}

async function install(
  registry: ReturnType<typeof createIntegrationRegistry>,
  order: string[] = []
) {
  return registry.install({
    activateCore: () => undefined,
    publish: () => order.push('publish'),
    drainPreload: () => order.push('drain'),
  });
}

afterEach(() => {
  vi.useRealTimers();
  document.head.replaceChildren();
  Object.defineProperty(document, 'currentScript', { configurable: true, value: null });
});

describe('integration manifest and registration admission', () => {
  it('offers one synchronous activation/commit barrier after every preparation completes', async () => {
    const order: string[] = [];
    let adoption: unknown;
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepareSync: () => {
          throw new Error('agent takeover must not use prepareSync');
        },
        prepare: () => {
          order.push('module:prepare');
          return {
            activate: ({ adoption: received, afterCommit }) => {
              expect(received).toBe(adoption);
              order.push('module:activate');
              afterCommit(() => order.push('after-commit'));
            },
          };
        },
      })
    );

    const result = await registry.install({
      prepareCore: () => order.push('core:prepare'),
      activateCore: () => order.push('core:activate'),
      publish: () => order.push('publish'),
      drainPreload: () => order.push('drain'),
      coordinateTakeover: (prepared) => {
        expect(Object.isFrozen(prepared)).toBe(true);
        expect(order).toEqual(['core:prepare', 'module:prepare']);
        const handoff = prepared.validateHandoff(
          {
            version: 1,
            releaseId: RELEASE_ID,
            generation: 1,
            projectionDigest: 'b'.repeat(64),
            integrationConfigDigest: 'c'.repeat(64),
            slices: ['first_display'],
            slots: [
              {
                id: 'slot-1',
                aliases: [],
                domId: 'div-1',
                gamPath: '/123/slot-1',
                formats: [[300, 250]],
                owner: 'trusted_server',
                outcome: 'failed',
                targeting: [],
                targetingOwnership: [],
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
            parserState: [],
            gptDiagnostics: { facts: [], overflowCount: 0, dropCount: 0 },
            timing: {
              bidsScriptMs: 0,
              firstDisplayMs: null,
              terminalMs: 0,
              paintMs: 0,
            },
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
              nextGlobalSlotOrdinal: 2,
              slots: [{ slotId: 'slot-1', impressions: 0, bindings: [] }],
            },
            mutationRevision: 0,
          },
          {
            version: 1,
            releaseId: RELEASE_ID,
            generation: 1,
            projectionDigest: 'b'.repeat(64),
            integrationConfigDigest: 'c'.repeat(64),
            slices: ['first_display'],
            slotCount: 1,
            outcomeCount: 1,
            capabilities: [],
            objectKinds: [],
          }
        );
        expect(handoff).toBeDefined();
        adoption = Object.freeze({
          version: 1 as const,
          adoptInitialDisplay: true as const,
          handoff: handoff!,
          identities: Object.freeze([]),
        });
        expect(snapshotPersistentFirstDisplayAdoptionV1(adoption)).toBe(adoption);
        prepared.activate(adoption);
        expect(() => prepared.activate()).toThrow();
        expect(order).toEqual([
          'core:prepare',
          'module:prepare',
          'core:activate',
          'module:activate',
        ]);
        prepared.commit();
      },
    });

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'core:prepare',
      'module:prepare',
      'core:activate',
      'module:activate',
      'publish',
      'after-commit',
      'drain',
    ]);
  });

  it('prepares, activates, and commits a no-agent runtime without yielding', async () => {
    const order: string[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    for (const id of ['gpt', 'prebid']) {
      expect(
        registry.register(
          registration(id, {
            prepareSync: () => {
              order.push(`prepareSync:${id}`);
              return Object.freeze({
                activate: () => order.push(`activate:${id}`),
              });
            },
            prepare: () => {
              throw new Error('no-agent runtime must not use prepare');
            },
          })
        )
      ).toBe(true);
    }
    queueMicrotask(() => order.push('microtask'));

    const result = registry.installSync({
      activateCore: () => order.push('activate:core'),
      publish: () => order.push('publish'),
      drainPreload: () => order.push('drain'),
    });

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'prepareSync:gpt',
      'prepareSync:prebid',
      'activate:core',
      'activate:gpt',
      'activate:prebid',
      'publish',
      'drain',
    ]);
    await Promise.resolve();
    expect(order[order.length - 1]).toBe('microtask');
  });

  it('rejects a thenable returned by no-agent prepareSync before activation', () => {
    const activate = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepareSync: () => Promise.resolve(Object.freeze({ activate })) as never,
      })
    );

    expect(
      registry.installSync({
        activateCore: activate,
        publish: vi.fn(),
        drainPreload: vi.fn(),
      })
    ).toEqual({ state: 'fallback', reason: 'bundle_partial' });
    expect(activate).not.toHaveBeenCalled();
  });

  it('fails closed when a takeover coordinator returns without committing', async () => {
    const activate = vi.fn();
    const publish = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () => ({ activate }),
      })
    );

    await expect(
      registry.install({
        activateCore: () => undefined,
        publish,
        drainPreload: () => undefined,
        coordinateTakeover: () => undefined,
      })
    ).resolves.toEqual({ state: 'fallback', reason: 'bundle_partial' });
    expect(activate).not.toHaveBeenCalled();
    expect(publish).not.toHaveBeenCalled();
  });

  it('accepts only the exact six-field takeover registrar ABI', () => {
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    const exact = registration('gpt');

    expect(Reflect.ownKeys(exact)).toEqual([
      'abi',
      'id',
      'phase',
      'releaseId',
      'prepareSync',
      'prepare',
    ]);
    expect(registry.register(exact)).toBe(true);
  });

  it('admits the exact five-field deferred shape and rejects prepareSync on deferred code', () => {
    const deferred = Object.freeze({
      abi: 1 as const,
      id: 'gpt_later',
      phase: 'deferred' as const,
      releaseId: RELEASE_ID,
      prepare: () => Object.freeze({ activate: () => undefined }),
    });

    expect(snapshotIntegrationRegistration(deferred)).toMatchObject({
      id: 'gpt_later',
      phase: 'deferred',
    });
    expect(
      snapshotIntegrationRegistration({ ...deferred, prepareSync: deferred.prepare })
    ).toBeUndefined();
  });

  it.each([
    ['old three-field ABI', { id: 'gpt', release: RELEASE_ID, prepare: vi.fn() }],
    ['missing abi', { id: 'gpt', phase: 'takeover', releaseId: RELEASE_ID, prepare: vi.fn() }],
    ['missing prepareSync', { ...registration('gpt'), prepareSync: undefined }],
    ['unknown field', { ...registration('gpt'), unexpected: true }],
    ['wrong phase', { ...registration('gpt'), phase: 'deferred' }],
    ['custom prototype', Object.assign(Object.create({ inherited: true }), registration('gpt'))],
    ['null prototype', Object.assign(Object.create(null), registration('gpt'))],
  ])('rejects %s without invoking module code', async (_name, candidate) => {
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(registry.register(candidate)).toBe(false);
    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
    const prepare = (candidate as { prepare?: unknown }).prepare;
    if (vi.isMockFunction(prepare)) expect(prepare).not.toHaveBeenCalled();
  });

  it('authenticates every takeover registration to the captured connected core script', () => {
    const runtimeScript = document.createElement('script');
    runtimeScript.id = 'trustedserver-js';
    runtimeScript.src = `${window.location.origin}/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;
    document.head.append(runtimeScript);
    Object.defineProperty(document, 'currentScript', {
      configurable: true,
      value: runtimeScript,
    });
    const registry = createIntegrationRegistryOwner({
      catalog: Object.freeze([
        Object.freeze({
          id: 'gpt',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze([]),
          provides: Object.freeze([]),
        }),
      ]),
      takeoverScript: runtimeScript,
      document,
      knownIntegrationIds: Object.freeze(['gpt']),
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(registry.register(registration('gpt'))).toBe(true);
  });

  it.each(['different current script', 'disconnected script', 'wrong exact source'])(
    'rejects a takeover registration from a %s',
    (failure) => {
      const runtimeScript = document.createElement('script');
      runtimeScript.id = 'trustedserver-js';
      runtimeScript.src = `${window.location.origin}/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;
      document.head.append(runtimeScript);
      Object.defineProperty(document, 'currentScript', {
        configurable: true,
        value: runtimeScript,
      });
      const registry = createIntegrationRegistryOwner({
        catalog: Object.freeze([
          Object.freeze({
            id: 'gpt',
            phase: 'takeover' as const,
            trigger: null,
            consumes: Object.freeze([]),
            provides: Object.freeze([]),
          }),
        ]),
        takeoverScript: runtimeScript,
        document,
        knownIntegrationIds: Object.freeze(['gpt']),
        manifest: manifest(['gpt']),
        releaseId: RELEASE_ID,
        startedAtMs: 0,
        now: () => 0,
      });
      if (failure === 'different current script') {
        Object.defineProperty(document, 'currentScript', {
          configurable: true,
          value: document.createElement('script'),
        });
      } else if (failure === 'disconnected script') {
        runtimeScript.remove();
      } else {
        runtimeScript.src = `${window.location.origin}/static/tsjs=tsjs-unified.min.js?v=${'d'.repeat(64)}`;
      }

      expect(registry.register(registration('gpt'))).toBe(false);
      expect(registry.state).toBe('failed');
    }
  );

  it('exposes only a frozen facade while mutable registry state stays in a closure', () => {
    const registry = createIntegrationRegistry({
      manifest: manifest([]),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(Object.isFrozen(registry)).toBe(true);
    expect(Reflect.ownKeys(registry).sort()).toEqual([
      'dispose',
      'install',
      'installSync',
      'manifest',
      'prepareDeferred',
      'register',
      'state',
    ]);
    expect('registrations' in registry).toBe(false);
    expect('prepared' in registry).toBe(false);
    registry.dispose();
  });

  it('rejects an integration array with executable iteration without invoking it', async () => {
    const iterator = vi.fn(function* () {
      for (let index = 0; index < 21; index += 1) {
        yield { id: `module_${index}`, phase: 'takeover' };
      }
    });
    const integrations: unknown[] = [];
    Object.defineProperty(integrations, Symbol.iterator, { value: iterator });
    const registry = createIntegrationRegistry({
      manifest: { version: 1, releaseId: RELEASE_ID, integrations },
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(iterator).not.toHaveBeenCalled();
    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
  });

  it.each([
    ['non-object', null],
    ['wrong version', { ...manifest([]), version: 2 }],
    ['extra manifest field', { ...manifest([]), unexpected: true }],
    ['wrong release grammar', { ...manifest([]), releaseId: 'ABC' }],
    ['malformed id', { ...manifest([]), integrations: [{ id: 'Uppercase', required: true }] }],
    [
      'unknown integration field',
      { ...manifest([]), integrations: [{ id: 'gpt', required: true, optional: false }] },
    ],
    ['non-required entry', { ...manifest([]), integrations: [{ id: 'gpt', required: false }] }],
    [
      'duplicate id',
      {
        ...manifest([]),
        integrations: [
          { id: 'gpt', required: true },
          { id: 'gpt', required: true },
        ],
      },
    ],
    [
      'over generated capacity',
      manifest(
        Array.from({ length: EMBEDDED_MAX_MANIFEST_MODULES + 1 }, (_, index) => `module_${index}`)
      ),
    ],
  ])('rejects a malformed manifest: %s', async (_name, candidate) => {
    const registry = createIntegrationRegistry({
      manifest: candidate,
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(registry.register(registration('gpt'))).toBe(false);
    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
  });

  it('requires the embedded release, manifest release, and bundle release to match', async () => {
    const registry = createIntegrationRegistry({
      manifest: { ...manifest(['gpt']), releaseId: OTHER_RELEASE_ID },
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(registry.register(registration('gpt'))).toBe(false);
    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
  });

  it('rejects a syntactically valid manifest id outside the frozen core bundle inventory', async () => {
    const prepare = vi.fn(() => ({ activate: () => undefined }));
    const registry = createIntegrationRegistry({
      manifest: manifest(['evil']),
      releaseId: RELEASE_ID,
      knownIntegrationIds: Object.freeze(['gpt']),
      startedAtMs: 0,
      now: () => 0,
    });

    expect(registry.register(registration('evil', { prepare }))).toBe(false);
    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
    expect(prepare).not.toHaveBeenCalled();
  });

  it.each([
    ['unknown id', registration('unknown')],
    ['wrong bundle release', registration('gpt', { releaseId: OTHER_RELEASE_ID })],
  ])('quarantines %s before prepare is called', async (_name, candidate) => {
    const prepare = vi.fn(candidate.prepare);
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(registry.register({ ...candidate, prepare })).toBe(false);
    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
    expect(prepare).not.toHaveBeenCalled();
  });

  it('rejects registration accessors without invoking bundle code during collection', async () => {
    const prepareGetter = vi.fn(() => () => ({ activate: () => undefined }));
    const candidate = Object.defineProperties(
      {},
      {
        id: { value: 'gpt', enumerable: true },
        release: { value: RELEASE_ID, enumerable: true },
        prepare: { get: prepareGetter, enumerable: true },
      }
    );
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(registry.register(candidate)).toBe(false);
    expect(prepareGetter).not.toHaveBeenCalled();
    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
  });

  it('rejects duplicate registration without invoking either module', async () => {
    const firstPrepare = vi.fn(() => ({ activate: () => undefined }));
    const secondPrepare = vi.fn(() => ({ activate: () => undefined }));
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(registry.register(registration('gpt', { prepare: firstPrepare }))).toBe(true);
    expect(registry.register(registration('gpt', { prepare: secondPrepare }))).toBe(false);
    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
    expect(firstPrepare).not.toHaveBeenCalled();
    expect(secondPrepare).not.toHaveBeenCalled();
  });

  it('rejects a takeover registration that skips the next manifest entry', async () => {
    const prepare = vi.fn(() => ({ activate: () => undefined }));
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(registry.register(registration('prebid', { prepare }))).toBe(false);
    expect(registry.state).toBe('failed');
    expect(prepare).not.toHaveBeenCalled();
    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
  });

  it('snapshots accepted registration code so retained objects cannot swap it later', async () => {
    const acceptedPrepare = vi.fn(() => ({ activate: () => undefined }));
    const swappedPrepare = vi.fn(() => ({
      activate: () => {
        throw new Error('must never execute');
      },
    }));
    const candidate = {
      abi: 1 as const,
      id: 'gpt',
      phase: 'takeover' as const,
      releaseId: RELEASE_ID,
      prepareSync: acceptedPrepare,
      prepare: acceptedPrepare,
    };
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    expect(registry.register(candidate)).toBe(true);
    candidate.id = 'unknown';
    candidate.releaseId = OTHER_RELEASE_ID;
    candidate.prepareSync = swappedPrepare;
    candidate.prepare = swappedPrepare;

    await expect(install(registry)).resolves.toMatchObject({ state: 'kernel' });
    expect(acceptedPrepare).toHaveBeenCalledTimes(1);
    expect(swappedPrepare).not.toHaveBeenCalled();
  });

  it('waits for required modules registered after install starts without early execution', async () => {
    const order: string[] = [];
    const gptPrepare = vi.fn(() => {
      order.push('prepare:gpt');
      return { activate: () => order.push('activate:gpt') };
    });
    const prebidPrepare = vi.fn(() => {
      order.push('prepare:prebid');
      return { activate: () => order.push('activate:prebid') };
    });
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(registration('gpt', { prepare: gptPrepare }));

    const installed = install(registry, order);
    await Promise.resolve();
    expect(registry.state).toBe('collecting');
    expect(order).toEqual([]);
    expect(registry.register(registration('prebid', { prepare: prebidPrepare }))).toBe(true);

    await expect(installed).resolves.toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'prepare:gpt',
      'prepare:prebid',
      'activate:gpt',
      'activate:prebid',
      'publish',
      'drain',
    ]);
  });

  it('fails missing required modules only at the shared boot deadline', async () => {
    vi.useFakeTimers();
    let now = 0;
    const prepare = vi.fn(() => ({ activate: () => undefined }));
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => now,
    });
    registry.register(registration('gpt', { prepare }));

    const installed = install(registry);
    await vi.advanceTimersByTimeAsync(9_999);
    expect(registry.state).toBe('collecting');
    expect(prepare).not.toHaveBeenCalled();
    now = 10_000;
    await vi.advanceTimersByTimeAsync(1);

    await expect(installed).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(prepare).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('accepts exactly 14 takeover modules in manifest order', async () => {
    const ids = Array.from({ length: 14 }, (_, index) => `module_${index}`);
    const order: string[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(ids),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    for (const id of ids) {
      expect(
        registry.register(
          registration(id, {
            prepare: () => {
              order.push(`prepare:${id}`);
              return { activate: () => order.push(`activate:${id}`) };
            },
          })
        )
      ).toBe(true);
    }

    await expect(install(registry, order)).resolves.toMatchObject({ state: 'kernel' });
    expect(order.slice(0, 14)).toEqual(ids.map((id) => `prepare:${id}`));
    expect(order.slice(14, 28)).toEqual(ids.map((id) => `activate:${id}`));
    expect(order.slice(28)).toEqual(['publish', 'drain']);
  });
});

describe('integration preparation and activation transaction', () => {
  it('stages only declared provider capabilities for later takeover consumers', async () => {
    const gpt = Object.freeze({ kind: 'gpt' });
    let consumerInterfaces: Readonly<Record<string, unknown>> | undefined;
    const registry = createIntegrationRegistryOwner({
      catalog: Object.freeze([
        Object.freeze({
          id: 'gpt',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze(['runtime.v1']),
          provides: Object.freeze(['gpt.v1']),
        }),
        Object.freeze({
          id: 'prebid',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze(['gpt.v1']),
          provides: Object.freeze([]),
        }),
      ]),
      knownIntegrationIds: Object.freeze(['gpt', 'prebid']),
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      runtimeCapability: Object.freeze({ kind: 'runtime' }),
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: ({ interfaces }) => {
          expect(Reflect.ownKeys(interfaces)).toEqual(['runtime.v1']);
          return { activate: () => undefined, interfaces: Object.freeze({ 'gpt.v1': gpt }) };
        },
      })
    );
    registry.register(
      registration('prebid', {
        prepare: ({ interfaces }) => {
          consumerInterfaces = interfaces;
          return { activate: () => undefined, interfaces: Object.freeze({}) };
        },
      })
    );

    await expect(install(registry)).resolves.toMatchObject({ state: 'kernel' });
    expect(consumerInterfaces).toEqual(Object.freeze({ 'gpt.v1': gpt }));
    expect(Object.isFrozen(consumerInterfaces)).toBe(true);
    expect(Reflect.ownKeys(consumerInterfaces ?? {})).toEqual(['gpt.v1']);
  });

  it('prepares a deferred consumer from committed takeover capabilities only', async () => {
    const gpt = Object.freeze({ kind: 'gpt' });
    const registry = createIntegrationRegistryOwner({
      catalog: Object.freeze([
        Object.freeze({
          id: 'gpt',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze([]),
          provides: Object.freeze(['gpt.v1']),
        }),
        Object.freeze({
          id: 'gpt_later',
          phase: 'deferred' as const,
          trigger: 'first_display_or_idle' as const,
          consumes: Object.freeze(['gpt.v1']),
          provides: Object.freeze([]),
        }),
      ]),
      knownIntegrationIds: Object.freeze(['gpt', 'gpt_later']),
      manifest: {
        ...manifest(['gpt']),
        integrations: Object.freeze([
          Object.freeze({ id: 'gpt', phase: 'takeover' as const }),
          Object.freeze({
            id: 'gpt_later',
            phase: 'deferred' as const,
            trigger: 'first_display_or_idle' as const,
            src: `/static/tsjs=tsjs-gpt_later.min.js?v=${'d'.repeat(64)}`,
          }),
        ]),
      },
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () => ({
          activate: () => undefined,
          interfaces: Object.freeze({ 'gpt.v1': gpt }),
        }),
      })
    );
    await expect(install(registry)).resolves.toMatchObject({ state: 'kernel' });

    const prepare = vi.fn(({ interfaces }: IntegrationPrepareContext) => {
      expect(interfaces).toEqual(Object.freeze({ 'gpt.v1': gpt }));
      return { activate: () => undefined };
    });
    const prepared = registry.prepareDeferred(
      { ...registration('gpt_later', { prepare }), phase: 'deferred' },
      Object.freeze({
        signal: new AbortController().signal,
        onDispose: vi.fn(),
      })
    );

    expect(prepare).toHaveBeenCalledTimes(1);
    expect(prepared).toMatchObject({ activate: expect.any(Function) });
  });

  it.each([
    ['missing declared key', Object.freeze({})],
    ['unknown key', Object.freeze({ 'gpt.v1': Object.freeze({}), 'other.v1': Object.freeze({}) })],
    ['mutable facade', Object.freeze({ 'gpt.v1': {} })],
    [
      'custom facade prototype',
      Object.freeze({ 'gpt.v1': Object.freeze(Object.create({ inherited: true })) }),
    ],
  ])('rejects provider interfaces with a %s', async (_name, interfaces) => {
    const prepareConsumer = vi.fn(() => ({
      activate: () => undefined,
      interfaces: Object.freeze({}),
    }));
    const registry = createIntegrationRegistryOwner({
      catalog: Object.freeze([
        Object.freeze({
          id: 'gpt',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze([]),
          provides: Object.freeze(['gpt.v1']),
        }),
        Object.freeze({
          id: 'prebid',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze(['gpt.v1']),
          provides: Object.freeze([]),
        }),
      ]),
      knownIntegrationIds: Object.freeze(['gpt', 'prebid']),
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () => ({ activate: () => undefined, interfaces }),
      })
    );
    registry.register(registration('prebid', { prepare: prepareConsumer }));

    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(prepareConsumer).not.toHaveBeenCalled();
  });

  it('prepares core-owned bindings before module preparation and activates afterward', async () => {
    const order: string[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => {
        order.push('bindings');
        return { config: Object.freeze({}), interfaces: Object.freeze({}) };
      },
    });
    registry.register(
      registration('gpt', {
        prepare: () => {
          order.push('module:prepare');
          return { activate: () => order.push('module:activate') };
        },
      })
    );

    const result = await registry.install({
      prepareCore: () => order.push('core:prepare'),
      activateCore: () => order.push('core:activate'),
      publish: () => order.push('publish'),
      drainPreload: () => order.push('drain'),
    });

    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'core:prepare',
      'bindings',
      'module:prepare',
      'core:activate',
      'module:activate',
      'publish',
      'drain',
    ]);
  });

  it('unwinds core-prepared resources when later module preparation fails', async () => {
    const release = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () => {
          throw new Error('fictional preparation failure');
        },
      })
    );

    const result = await registry.install({
      prepareCore: ({ onDispose }) => onDispose(release),
      activateCore: vi.fn(),
      publish: vi.fn(),
      drainPreload: vi.fn(),
    });

    expect(result).toMatchObject({ state: 'fallback', reason: 'bundle_partial' });
    expect(release).toHaveBeenCalledTimes(1);
  });

  it('collects without execution, prepares sequentially, and commits in exact order', async () => {
    const order: string[] = [];
    const contexts: IntegrationPrepareContext[] = [];
    let finishGpt: (() => void) | undefined;
    const gptPrepared = new Promise<void>((resolve) => {
      finishGpt = resolve;
    });
    const frozenConfig = Object.freeze({ enabled: true });
    const frozenInterfaces = Object.freeze({ adapter: Object.freeze({ kind: 'fake' }) });
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
      getBindings: (id) => ({
        config: id === 'gpt' ? frozenConfig : Object.freeze({ enabled: false }),
        interfaces: frozenInterfaces,
      }),
    });
    registry.register(
      registration('gpt', {
        prepare: async (context) => {
          contexts.push(context);
          order.push('prepare:gpt:start');
          await gptPrepared;
          order.push('prepare:gpt:end');
          return {
            activate: (activation) => {
              order.push('activate:gpt');
              activation.afterCommit(() => order.push('after:gpt'));
            },
          };
        },
      })
    );
    registry.register(
      registration('prebid', {
        prepare: (context) => {
          contexts.push(context);
          order.push('prepare:prebid');
          return {
            activate: (activation) => {
              order.push('activate:prebid');
              activation.afterCommit(() => order.push('after:prebid'));
            },
          };
        },
      })
    );

    expect(order).toEqual([]);
    const installed = install(registry, order);
    await vi.waitFor(() => expect(order).toEqual(['prepare:gpt:start']));
    expect(order).not.toContain('prepare:prebid');
    finishGpt?.();

    await expect(installed).resolves.toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'prepare:gpt:start',
      'prepare:gpt:end',
      'prepare:prebid',
      'activate:gpt',
      'activate:prebid',
      'publish',
      'after:gpt',
      'after:prebid',
      'drain',
    ]);
    expect(contexts).toHaveLength(2);
    expect(Object.isFrozen(contexts[0])).toBe(true);
    expect(contexts[0]?.config).toBe(frozenConfig);
    expect(contexts[0]?.interfaces).toBe(frozenInterfaces);
  });

  it('closes a synchronous preparation context before detached microtasks can use it', async () => {
    const lateDisposer = vi.fn();
    let lateError: unknown;
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: (context) => {
          queueMicrotask(() => {
            try {
              context.onDispose(lateDisposer);
            } catch (error) {
              lateError = error;
            }
          });
          return { activate: () => undefined };
        },
      })
    );

    await expect(install(registry)).resolves.toMatchObject({ state: 'kernel' });
    await Promise.resolve();
    expect(lateError).toBeInstanceOf(Error);
    expect(lateDisposer).not.toHaveBeenCalled();
  });

  it('rejects a prepared activation accessor without invoking it or publishing', async () => {
    const owner = new AbortController();
    const activateGetter = vi.fn(() => {
      owner.abort();
      return () => undefined;
    });
    const publish = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
      signal: owner.signal,
    });
    registry.register(
      registration('gpt', {
        prepare: () =>
          Object.defineProperty({}, 'activate', {
            get: activateGetter,
            enumerable: true,
          }) as { activate: () => void },
      })
    );

    const result = await registry.install({
      activateCore: () => undefined,
      publish,
      drainPreload: vi.fn(),
    });

    expect(result).toMatchObject({ state: 'fallback', reason: 'bundle_partial' });
    expect(activateGetter).not.toHaveBeenCalled();
    expect(publish).not.toHaveBeenCalled();
  });

  it('rejects a frozen interface container that exposes a mutable adapter facade', async () => {
    const prepare = vi.fn(() => ({ activate: () => undefined }));
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
      getBindings: () => ({
        config: Object.freeze({}),
        interfaces: Object.freeze({ adapter: { mutable: true } }),
      }),
    });
    registry.register(registration('gpt', { prepare }));

    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(prepare).not.toHaveBeenCalled();
  });

  it('snapshots each prepared activation before preparing a later module', async () => {
    const acceptedActivate = vi.fn();
    const swappedActivate = vi.fn(() => {
      throw new Error('must never execute');
    });
    const prepared = { activate: acceptedActivate };
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(registration('gpt', { prepare: () => prepared }));
    registry.register(
      registration('prebid', {
        prepare: () => {
          prepared.activate = swappedActivate;
          return { activate: () => undefined };
        },
      })
    );

    await expect(install(registry)).resolves.toMatchObject({ state: 'kernel' });
    expect(acceptedActivate).toHaveBeenCalledTimes(1);
    expect(swappedActivate).not.toHaveBeenCalled();
  });

  it.each([
    [
      'synchronous throw',
      () => {
        throw new Error('fictional prepare throw');
      },
    ],
    ['asynchronous rejection', () => Promise.reject(new Error('fictional prepare rejection'))],
  ])('unwinds a preparation %s as bundle_partial', async (_name, prepare) => {
    const disposed: string[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: (context) => {
          context.onDispose(() => disposed.push('prepared'));
          return prepare();
        },
      })
    );

    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(disposed).toEqual(['prepared']);
  });

  it('aborts a pending preparation at the shared deadline and ignores its late continuation', async () => {
    vi.useFakeTimers();
    let now = 0;
    let finishPrepare: ((value: { activate: () => void }) => void) | undefined;
    let context: IntegrationPrepareContext | undefined;
    const activate = vi.fn();
    const lateDispose = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => now,
    });
    registry.register(
      registration('gpt', {
        prepare: (receivedContext) => {
          context = receivedContext;
          return new Promise((resolve) => {
            finishPrepare = resolve;
          });
        },
      })
    );

    const installed = install(registry);
    await vi.advanceTimersByTimeAsync(9_999);
    expect(registry.state).toBe('preparing');
    now = 10_000;
    await vi.advanceTimersByTimeAsync(1);
    await expect(installed).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(context?.signal.aborted).toBe(true);
    context?.onDispose(lateDispose);
    expect(lateDispose).toHaveBeenCalledTimes(1);

    finishPrepare?.({ activate });
    await Promise.resolve();
    expect(activate).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('aborts preparation through the caller signal and leaves no late activation', async () => {
    const owner = new AbortController();
    const activate = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
      signal: owner.signal,
    });
    registry.register(
      registration('gpt', {
        prepare: ({ signal }) =>
          new Promise((resolve) => {
            signal.addEventListener('abort', () => resolve({ activate }));
          }),
      })
    );

    const installed = install(registry);
    owner.abort();
    await expect(installed).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(activate).not.toHaveBeenCalled();
  });

  it('observes a rejected preparation promise returned after synchronous abort', async () => {
    const owner = new AbortController();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
      signal: owner.signal,
    });
    registry.register(
      registration('gpt', {
        prepare: () => {
          owner.abort();
          return Promise.reject(new Error('fictional late preparation rejection'));
        },
      })
    );

    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    await Promise.resolve();
  });

  it('turns a registration attempt during preparation into abi_mismatch', async () => {
    let finishPrepare: ((value: { activate: () => void }) => void) | undefined;
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () =>
          new Promise((resolve) => {
            finishPrepare = resolve;
          }),
      })
    );

    const installed = install(registry);
    await vi.waitFor(() => expect(registry.state).toBe('preparing'));
    expect(registry.register(registration('unknown'))).toBe(false);
    finishPrepare?.({ activate: () => undefined });

    await expect(installed).resolves.toMatchObject({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
  });

  it('unwinds activated and prepared resources in reverse order on activation failure', async () => {
    const order: string[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    for (const id of ['gpt', 'prebid']) {
      registry.register(
        registration(id, {
          prepare: (preparation) => {
            preparation.onDispose(() => order.push(`dispose:prepare:${id}`));
            return {
              activate: (activation) => {
                activation.onDispose(() => order.push(`dispose:activate:${id}`));
                order.push(`activate:${id}`);
                if (id === 'prebid') throw new Error('fictional activation failure');
              },
            };
          },
        })
      );
    }

    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(order).toEqual([
      'activate:gpt',
      'activate:prebid',
      'dispose:activate:prebid',
      'dispose:prepare:prebid',
      'dispose:activate:gpt',
      'dispose:prepare:gpt',
    ]);
  });

  it('activates reversible core effects first and unwinds them after every module', async () => {
    const order: string[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    for (const id of ['gpt', 'prebid']) {
      registry.register(
        registration(id, {
          prepare: () => ({
            activate: ({ onDispose }) => {
              onDispose(() => order.push(`dispose:${id}`));
              order.push(`activate:${id}`);
              if (id === 'prebid') throw new Error('fictional later activation failure');
            },
          }),
        })
      );
    }

    const result = await registry.install({
      activateCore: ({ onDispose }) => {
        onDispose(() => order.push('dispose:core'));
        order.push('activate:core');
      },
      publish: () => order.push('publish'),
      drainPreload: () => order.push('drain'),
    });

    expect(result).toMatchObject({ state: 'fallback', reason: 'bundle_partial' });
    expect(order).toEqual([
      'activate:core',
      'activate:gpt',
      'activate:prebid',
      'dispose:prebid',
      'dispose:gpt',
      'dispose:core',
    ]);
  });

  it.each([
    ['deadline crossing', ({ setNow }: { setNow: (value: number) => void }) => setNow(10_000)],
    ['async rejection', () => Promise.reject(new Error('fictional core rejection'))],
  ])('rejects a core activation %s before module activation', async (_name, activate) => {
    let now = 0;
    const moduleActivate = vi.fn();
    const publish = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => now,
    });
    registry.register(registration('gpt', { prepare: () => ({ activate: moduleActivate }) }));

    const result = await registry.install({
      activateCore: () => activate({ setNow: (value) => (now = value) }),
      publish,
      drainPreload: vi.fn(),
    });

    expect(result).toMatchObject({ state: 'fallback', reason: 'bundle_partial' });
    expect(moduleActivate).not.toHaveBeenCalled();
    expect(publish).not.toHaveBeenCalled();
  });

  it('cannot commit after activation synchronously aborts the owner', async () => {
    const owner = new AbortController();
    const live = { wrapper: 'publisher' };
    const publish = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
      signal: owner.signal,
    });
    registry.register(
      registration('gpt', {
        prepare: () => ({
          activate: ({ onDispose }) => {
            const previous = live.wrapper;
            onDispose(() => {
              if (live.wrapper === 'tsjs') live.wrapper = previous;
            });
            live.wrapper = 'tsjs';
            owner.abort();
            live.wrapper = 'tsjs';
          },
        }),
      })
    );

    const result = await registry.install({
      activateCore: () => undefined,
      publish,
      drainPreload: vi.fn(),
    });

    expect(result).toMatchObject({ state: 'fallback', reason: 'bundle_partial' });
    expect(publish).not.toHaveBeenCalled();
    expect(live.wrapper).toBe('publisher');
  });

  it('cannot commit after an activation attempts late bundle registration', async () => {
    const live = { wrapper: 'publisher' };
    const publish = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () => ({
          activate: ({ onDispose }) => {
            const previous = live.wrapper;
            onDispose(() => {
              if (live.wrapper === 'tsjs') live.wrapper = previous;
            });
            live.wrapper = 'tsjs';
            expect(registry.register(registration('unknown'))).toBe(false);
            live.wrapper = 'tsjs';
          },
        }),
      })
    );

    const result = await registry.install({
      activateCore: () => undefined,
      publish,
      drainPreload: vi.fn(),
    });

    expect(result).toMatchObject({ state: 'fallback', reason: 'abi_mismatch' });
    expect(publish).not.toHaveBeenCalled();
    expect(live.wrapper).toBe('publisher');
  });

  it('restores reversible effects before fallback publication', async () => {
    const live = { wrapper: 'publisher' };
    const observations: string[] = [];
    const irreversibleWork = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () => {
          observations.push(`prepare:${live.wrapper}`);
          return {
            activate: ({ onDispose }) => {
              const previous = live.wrapper;
              onDispose(() => {
                if (live.wrapper === 'tsjs') live.wrapper = previous;
              });
              live.wrapper = 'tsjs';
            },
          };
        },
      })
    );
    registry.register(
      registration('prebid', {
        prepare: () => ({
          activate: ({ afterCommit }) => {
            afterCommit(irreversibleWork);
            throw new Error('later fictional failure');
          },
        }),
      })
    );

    const result = await registry.install({
      activateCore: () => undefined,
      publish: () => observations.push(`publish:${live.wrapper}`),
      drainPreload: () => observations.push('drain'),
    });

    expect(result).toMatchObject({ state: 'fallback' });
    expect(live.wrapper).toBe('publisher');
    expect(observations).toEqual(['prepare:publisher']);
    expect(irreversibleWork).not.toHaveBeenCalled();
  });

  it('rejects asynchronous kernel publication and observes its rejection', async () => {
    const drainPreload = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest([]),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });

    const result = await registry.install({
      activateCore: () => undefined,
      publish: async () => {
        throw new Error('fictional asynchronous publication rejection');
      },
      drainPreload,
    });

    expect(result).toMatchObject({ state: 'fallback', reason: 'bundle_partial' });
    expect(drainPreload).not.toHaveBeenCalled();
    await Promise.resolve();
  });

  it.each([9_999, 10_000, 10_001])(
    'checks the monotonic deadline after activation at %i ms',
    async (activationReturnMs) => {
      let now = 0;
      const order: string[] = [];
      const registry = createIntegrationRegistry({
        manifest: manifest(['gpt']),
        releaseId: RELEASE_ID,
        startedAtMs: 0,
        now: () => now,
      });
      const prepare = () => ({
        activate: () => {
          order.push('activate');
          now = activationReturnMs;
        },
      });
      registry.register(
        registration('gpt', {
          prepareSync: prepare,
          prepare,
        })
      );

      const result = registry.installSync({
        activateCore: () => undefined,
        publish: () => order.push('publish'),
        drainPreload: () => order.push('drain'),
      });
      if (activationReturnMs < 10_000) {
        expect(result).toMatchObject({ state: 'kernel' });
        expect(order).toEqual(['activate', 'publish', 'drain']);
      } else {
        expect(result).toMatchObject({ state: 'fallback', reason: 'bundle_partial' });
        expect(order).toEqual(['activate']);
      }
    }
  );

  it('checks the deadline again immediately before handoff', async () => {
    let checks = 0;
    const activateCore = vi.fn();
    const publish = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest([]),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => (checks++ < 5 ? 9_999 : 10_000),
    });

    expect(
      registry.installSync({
        activateCore,
        publish,
        drainPreload: vi.fn(),
      })
    ).toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(activateCore).toHaveBeenCalledOnce();
    expect(publish).not.toHaveBeenCalled();
    expect(checks).toBe(6);
  });

  it('treats an asynchronous activation as a synchronous barrier violation', async () => {
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () => ({
          activate: async () => {
            throw new Error('fictional async activation rejection');
          },
        }),
      })
    );

    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
  });

  it('turns a second afterCommit registration into bundle_partial', async () => {
    const staged = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () => ({
          activate: ({ afterCommit }) => {
            afterCommit(staged);
            afterCommit(staged);
          },
        }),
      })
    );

    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(staged).not.toHaveBeenCalled();
  });

  it('latches duplicate afterCommit as bundle_partial even when module code catches the throw', async () => {
    const first = vi.fn();
    const second = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () => ({
          activate: ({ afterCommit }) => {
            try {
              afterCommit(first);
              afterCommit(second);
            } catch {
              // A bundle cannot swallow a registry contract violation and commit.
            }
          },
        }),
      })
    );

    await expect(install(registry)).resolves.toMatchObject({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(first).not.toHaveBeenCalled();
    expect(second).not.toHaveBeenCalled();
  });

  it('isolates afterCommit failure to its module and keeps the committed kernel', async () => {
    const order: string[] = [];
    const runtimeFailures: unknown[] = [];
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt', 'prebid']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
      onRuntimeFailure: (failure) => runtimeFailures.push(failure),
    });
    registry.register(
      registration('gpt', {
        prepare: ({ onDispose }) => {
          onDispose(() => order.push('dispose:gpt'));
          return {
            activate: ({ afterCommit }) =>
              afterCommit(() => {
                order.push('after:gpt');
                throw new Error('fictional post-commit failure');
              }),
          };
        },
      })
    );
    registry.register(
      registration('prebid', {
        prepare: () => ({
          activate: ({ afterCommit }) => afterCommit(() => order.push('after:prebid')),
        }),
      })
    );

    const result = await install(registry, order);

    expect(result).toMatchObject({
      state: 'kernel',
      runtimeFailures: [{ id: 'gpt', phase: 'after_commit' }],
    });
    expect(runtimeFailures).toEqual([{ id: 'gpt', phase: 'after_commit' }]);
    expect(Object.isFrozen(runtimeFailures[0])).toBe(true);
    expect(order).toEqual(['publish', 'after:gpt', 'dispose:gpt', 'after:prebid', 'drain']);
    expect(registry.state).toBe('committed');
  });

  it('observes a rejecting asynchronous preload drain without undoing commit', async () => {
    const onDisposalError = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest([]),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
      onDisposalError,
    });

    const result = await registry.install({
      activateCore: () => undefined,
      publish: () => undefined,
      drainPreload: async () => {
        throw new Error('fictional asynchronous preload rejection');
      },
    });

    expect(result).toMatchObject({ state: 'kernel' });
    expect(registry.state).toBe('committed');
    await vi.waitFor(() => expect(onDisposalError).toHaveBeenCalledTimes(1));
    expect(registry.state).toBe('committed');
  });

  it('refuses late registration after fallback or commit without invoking module code', async () => {
    const fallbackRegistry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 10_000,
    });
    await install(fallbackRegistry);
    const fallbackPrepare = vi.fn();
    expect(fallbackRegistry.register(registration('gpt', { prepare: fallbackPrepare }))).toBe(
      false
    );

    const committedRegistry = createIntegrationRegistry({
      manifest: manifest([]),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    await install(committedRegistry);
    const committedPrepare = vi.fn();
    expect(committedRegistry.register(registration('gpt', { prepare: committedPrepare }))).toBe(
      false
    );

    expect(fallbackPrepare).not.toHaveBeenCalled();
    expect(committedPrepare).not.toHaveBeenCalled();
  });

  it('documents the same-thread limitation by completing only after activate returns', async () => {
    let returned = false;
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    registry.register(
      registration('gpt', {
        prepare: () => ({
          activate: () => {
            expect(registry.state).toBe('activating');
            returned = true;
          },
        }),
      })
    );

    await expect(install(registry)).resolves.toMatchObject({ state: 'kernel' });
    expect(returned).toBe(true);
  });

  it('memoizes installation before any synchronous callback can reenter it', async () => {
    const phases: string[] = [];
    const reentrantPromises: Promise<unknown>[] = [];
    const ignoredPublish = vi.fn();
    const ignoredDrain = vi.fn();
    const registry = createIntegrationRegistry({
      manifest: manifest(['gpt']),
      releaseId: RELEASE_ID,
      startedAtMs: 0,
      now: () => 0,
    });
    const reenter = () => {
      reentrantPromises.push(
        registry.install({
          activateCore: vi.fn(),
          publish: ignoredPublish,
          drainPreload: ignoredDrain,
        })
      );
    };
    registry.register(
      registration('gpt', {
        prepare: () => {
          phases.push('prepare');
          reenter();
          return {
            activate: () => {
              phases.push('activate');
              reenter();
            },
          };
        },
      })
    );

    const installed = registry.install({
      activateCore: () => {
        phases.push('core');
        reenter();
      },
      publish: () => {
        phases.push('publish');
        reenter();
      },
      drainPreload: () => phases.push('drain'),
    });

    await expect(installed).resolves.toMatchObject({ state: 'kernel' });
    expect(reentrantPromises).toHaveLength(4);
    for (const promise of reentrantPromises) expect(promise).toBe(installed);
    expect(phases).toEqual(['prepare', 'core', 'activate', 'publish', 'drain']);
    expect(ignoredPublish).not.toHaveBeenCalled();
    expect(ignoredDrain).not.toHaveBeenCalled();
  });
});
