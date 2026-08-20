import { afterEach, describe, expect, expectTypeOf, it, vi } from 'vitest';

import {
  AdUnitRegistrationError,
  RequestAdsInputError,
  TsjsUnavailableError,
  type AdUnitRegistrationErrorCode,
} from '../../src/kernel/fallback';
import { createRuntime as createRuntimeOwner, type RuntimeOptions } from '../../src/kernel/runtime';
import { snapshotTsjsBootV1 } from '../../src/core/contracts/boot';
import { createDiagnosticsPresentationIntegrationRegistration } from '../../src/integrations/gpt_diagnostics/presentation';
import type {
  IntegrationPrepareContext,
  PreparedIntegration,
} from '../../src/kernel/integration_registry';
import { createLifecycleIntegrationRegistration } from '../../src/kernel/lifecycle_module';

const RELEASE = 'a'.repeat(64);
const TRUSTED_RUNTIME_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;

function takeoverRegistration(
  candidate: Readonly<{
    abi: 1;
    id: string;
    phase: 'takeover';
    releaseId: string;
    prepare: (
      context: IntegrationPrepareContext
    ) => PreparedIntegration | PromiseLike<PreparedIntegration>;
  }>
) {
  return Object.freeze({
    abi: candidate.abi,
    id: candidate.id,
    phase: candidate.phase,
    releaseId: candidate.releaseId,
    prepareSync: (context: IntegrationPrepareContext) =>
      candidate.prepare(context) as PreparedIntegration,
    prepare: candidate.prepare,
  });
}

function installTestRuntimeScript(runtimeDocument: Document, takeover = false): void {
  if (runtimeDocument.currentScript) return;
  const script = runtimeDocument.createElement('script');
  script.id = takeover ? 'trustedserver-js-runtime' : 'trustedserver-js';
  script.src = new URL(TRUSTED_RUNTIME_SRC, runtimeDocument.location.origin).href;
  runtimeDocument.head.insertBefore(script, null);
  Object.defineProperty(runtimeDocument, 'currentScript', {
    configurable: true,
    value: script,
  });
}

function createRuntime(options: RuntimeOptions) {
  installTestRuntimeScript(options.document ?? document, options.coordinateTakeover !== undefined);
  let acceptedOptions = options;
  try {
    if (
      typeof options.boot === 'object' &&
      options.boot !== null &&
      !Array.isArray(options.boot) &&
      Object.getPrototypeOf(options.boot) === Object.prototype
    ) {
      const descriptors = Object.getOwnPropertyDescriptors(options.boot);
      if (Object.values(descriptors).every((descriptor) => 'value' in descriptor)) {
        const fields = Object.fromEntries(
          Object.entries(descriptors).map(([key, descriptor]) => [key, descriptor.value])
        );
        const carrier = Object.prototype.hasOwnProperty.call(fields, 'integrations')
          ? fields['integrations']
          : defaultIntegrationConfigs(options.manifest);
        const candidate = Object.prototype.hasOwnProperty.call(fields, 'abi')
          ? options.boot
          : {
              abi: 1,
              releaseId: RELEASE,
              manifest: options.manifest,
              auctionProjection: fields['auctionProjection'],
              integrations: carrier,
              creative: fields['creative'],
              diagnostics: fields['diagnostics'],
            };
        const snapshot = snapshotTsjsBootV1(candidate, RELEASE);
        if (snapshot) {
          const retained =
            Object.prototype.hasOwnProperty.call(fields, 'abi') && Object.isFrozen(options.boot);
          const acceptedBoot = retained ? options.boot : snapshot;
          acceptedOptions = {
            ...options,
            manifest: retained ? fields['manifest'] : snapshot.manifest,
            boot: acceptedBoot,
          };
        }
      }
    }
  } catch {
    // Hostile values remain untouched so production boundary tests can reject them.
  }
  return createRuntimeOwner(acceptedOptions);
}

const CONFIG_ORDER = Object.freeze([
  'aps',
  'datadome',
  'didomi',
  'google_tag_manager',
  'gpt',
  'lockr',
  'osano',
  'permutive',
  'prebid',
  'sourcepoint',
  'testlight',
]);

function configProduct(id: string): string | undefined {
  if (id === 'gpt' || id === 'gpt_later') return 'gpt';
  if (id === 'osano_consent' || id === 'osano_lifecycle') return 'osano';
  if (id === 'permutive_context' || id === 'permutive_lifecycle') return 'permutive';
  if (id === 'prebid' || id === 'prebid_later') return 'prebid';
  if (id === 'sourcepoint_consent' || id === 'sourcepoint_lifecycle') return 'sourcepoint';
  return CONFIG_ORDER.includes(id) ? id : undefined;
}

function defaultConfig(id: string): Readonly<Record<string, unknown>> {
  if (id === 'didomi') return { proxyPath: '/integrations/didomi/sdk.js' };
  if (id === 'gpt') return { gamAttributionEnabled: false };
  if (id === 'prebid') {
    return { accountId: 'test', timeout: 1_000, debug: false, bidders: [] };
  }
  if (id === 'sourcepoint') return { rewriteSdk: false };
  return {};
}

function defaultIntegrationConfigs(candidateManifest: unknown): Readonly<Record<string, unknown>> {
  const entries =
    typeof candidateManifest === 'object' &&
    candidateManifest !== null &&
    Array.isArray((candidateManifest as { integrations?: unknown }).integrations)
      ? (candidateManifest as { integrations: Array<{ id?: unknown }> }).integrations
      : [];
  const selected = new Set(
    entries.flatMap(({ id }) => (typeof id === 'string' ? [configProduct(id)] : []))
  );
  return {
    version: 1,
    entries: CONFIG_ORDER.filter((id) => selected.has(id)).map((id) => ({
      id,
      config: defaultConfig(id),
    })),
  };
}

function boot(results: readonly object[] = []) {
  return {
    auctionProjection: {
      version: 1,
      auction: { version: 1, auctionId: 'boot', results },
      slots: results.map((result) => {
        const slot = (result as { readonly slot?: unknown }).slot;
        return {
          slot,
          gamUnitPath: `/123/${String(slot)}`,
          divId: String(slot),
          formats: [[300, 250]],
          targeting: {},
        };
      }),
      bids: [],
    },
    creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
    diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
  };
}

function takeoverHandoff() {
  const projectionDigest = 'b'.repeat(64);
  return {
    handoff: {
      version: 1,
      releaseId: RELEASE,
      generation: 1,
      projectionDigest,
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
      timing: { bidsScriptMs: 0, firstDisplayMs: null, terminalMs: 0, paintMs: 0 },
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
    outline: {
      version: 1,
      releaseId: RELEASE,
      generation: 1,
      projectionDigest,
      integrationConfigDigest: 'c'.repeat(64),
      slices: ['first_display'],
      slotCount: 1,
      outcomeCount: 1,
      capabilities: [],
      objectKinds: [],
    },
  } as const;
}

function manifest(ids: readonly string[]) {
  const deferredIds = new Set([
    'diagnostics_presentation',
    'gpt_later',
    'osano_lifecycle',
    'permutive_lifecycle',
    'prebid_later',
    'sourcepoint_lifecycle',
  ]);
  return {
    version: 1,
    releaseId: RELEASE,
    firstDisplay: null,
    runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
    integrations: ids.map((id) =>
      deferredIds.has(id)
        ? {
            id,
            phase: 'deferred' as const,
            trigger: 'first_display_or_idle' as const,
            src: `/static/tsjs=tsjs-${id}.min.js?v=${'d'.repeat(64)}`,
          }
        : { id, phase: 'takeover' as const }
    ),
  };
}

type ReflectionTrap = 'getPrototypeOf' | 'ownKeys' | 'getOwnPropertyDescriptor';

function hostileRecord(trap: ReflectionTrap, target: object = {}): object {
  const fail = () => {
    throw new Error(`hostile ${trap}`);
  };
  const handler: ProxyHandler<object> = {};
  if (trap === 'getPrototypeOf') handler.getPrototypeOf = fail;
  if (trap === 'ownKeys') handler.ownKeys = fail;
  if (trap === 'getOwnPropertyDescriptor') handler.getOwnPropertyDescriptor = fail;
  return new Proxy(target, handler);
}

function thrownBy(callback: () => unknown): unknown {
  try {
    callback();
  } catch (error) {
    return error;
  }
  throw new Error('Expected callback to throw');
}

describe('Runtime bootstrap owner', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    document.head.replaceChildren();
    Object.defineProperty(document, 'currentScript', { configurable: true, value: null });
  });

  it('exports the exact programmatic registration error taxonomy', () => {
    type ExpectedCode =
      | 'invalid_units'
      | 'invalid_unit'
      | 'invalid_code'
      | 'duplicate_code'
      | 'slot_collision'
      | 'invalid_media_types'
      | 'invalid_dimensions'
      | 'dimensions_out_of_range'
      | 'invalid_bids'
      | 'invalid_bidder'
      | 'invalid_params'
      | 'request_body_too_large'
      | 'registry_capacity';

    expectTypeOf<AdUnitRegistrationErrorCode>().toEqualTypeOf<ExpectedCode>();
    expectTypeOf<AdUnitRegistrationError['code']>().toEqualTypeOf<ExpectedCode>();
  });

  it.each([
    {
      boundary: 'no document',
      arrange: () => {
        vi.stubGlobal('document', undefined);
        return undefined;
      },
    },
    {
      boundary: 'no takeover tag',
      arrange: () => document,
    },
    {
      boundary: 'wrong realm and owner document',
      arrange: () => {
        const frame = document.createElement('iframe');
        document.body.append(frame);
        const foreignDocument = frame.contentDocument;
        if (!foreignDocument) throw new Error('should expose an iframe document');
        const script = foreignDocument.createElement('script');
        script.id = 'trustedserver-js';
        script.src = new URL(TRUSTED_RUNTIME_SRC, window.location.origin).href;
        foreignDocument.head.append(script);
        Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
        return document;
      },
    },
    {
      boundary: 'wrong id',
      arrange: () => {
        const script = document.createElement('script');
        script.id = 'publisher-script';
        script.src = new URL(TRUSTED_RUNTIME_SRC, window.location.origin).href;
        document.head.append(script);
        Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
        return document;
      },
    },
    {
      boundary: 'disconnected tag',
      arrange: () => {
        const script = document.createElement('script');
        script.id = 'trustedserver-js';
        script.src = new URL(TRUSTED_RUNTIME_SRC, window.location.origin).href;
        Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
        return document;
      },
    },
    {
      boundary: 'duplicate tag',
      arrange: () => {
        const script = document.createElement('script');
        script.id = 'trustedserver-js';
        script.src = new URL(TRUSTED_RUNTIME_SRC, window.location.origin).href;
        const duplicate = script.cloneNode() as HTMLScriptElement;
        document.head.append(script, duplicate);
        Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
        return document;
      },
    },
    {
      boundary: 'cross-origin source',
      arrange: () => {
        const script = document.createElement('script');
        script.id = 'trustedserver-js';
        script.src = `https://attacker.example${TRUSTED_RUNTIME_SRC}`;
        document.head.append(script);
        Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
        return document;
      },
    },
    {
      boundary: 'fragment source',
      arrange: () => {
        const script = document.createElement('script');
        script.id = 'trustedserver-js';
        script.src = `${new URL(TRUSTED_RUNTIME_SRC, window.location.origin).href}#publisher`;
        document.head.append(script);
        Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
        return document;
      },
    },
    {
      boundary: 'wrong route',
      arrange: () => {
        const script = document.createElement('script');
        script.id = 'trustedserver-js';
        script.src = new URL(
          `/static/tsjs=tsjs-publisher.min.js?v=${'c'.repeat(64)}`,
          window.location.origin
        ).href;
        document.head.append(script);
        Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
        return document;
      },
    },
    {
      boundary: 'malformed artifact hash',
      arrange: () => {
        const script = document.createElement('script');
        script.id = 'trustedserver-js';
        script.src = new URL(
          `/static/tsjs=tsjs-unified.min.js?v=${'C'.repeat(64)}`,
          window.location.origin
        ).href;
        document.head.append(script);
        Object.defineProperty(document, 'currentScript', { configurable: true, value: script });
        return document;
      },
    },
  ])('rejects caller-supplied takeover source at the $boundary boundary', ({ arrange }) => {
    const runtimeDocument = arrange();
    const queued = vi.fn();
    const target = { boot: boot(), que: [queued] };
    const bootDescriptor = Object.getOwnPropertyDescriptor(target, 'boot');
    const queueDescriptor = Object.getOwnPropertyDescriptor(target, 'que');
    const options: RuntimeOptions & Record<string, unknown> = {
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([] as string[]),
      boot: target.boot,
      ...(runtimeDocument ? { document: runtimeDocument } : {}),
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    };
    options['trustedRuntimeSrc'] = TRUSTED_RUNTIME_SRC;
    const runtime = createRuntimeOwner(options);

    expect(runtime.start()).toBe(false);
    expect(runtime.state).toBe('unclaimed');
    expect(Object.getOwnPropertyDescriptor(target, 'boot')).toEqual(bootDescriptor);
    expect(Object.getOwnPropertyDescriptor(target, 'que')).toEqual(queueDescriptor);
    expect(target).not.toHaveProperty('_registerIntegration');
    expect(target).not.toHaveProperty('_internal');
    expect(queued).not.toHaveBeenCalled();
  });

  it('commits one kernel after core/integration activation and afterCommit before queue drain', async () => {
    const order: string[] = [];
    const target = { que: [() => order.push('queued')], config: { publisher: true } };
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      activateCore: () => order.push('core'),
      kernel: {
        addAdUnits: () => ({ registered: [] }),
        diagnostics: Object.freeze({}),
        requestAds: async () => ({ slots: [] }),
      },
    });

    expect(runtime.state).toBe('unclaimed');
    expect(runtime.start()).toBe(true);
    expect(runtime.state).toBe('installing');
    expect(target.config).toEqual({ publisher: true });
    expect(
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'gpt',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: () => ({
            activate: ({ afterCommit }: { afterCommit(callback: () => void): void }) => {
              order.push('integration');
              afterCommit(() => order.push('after-commit'));
            },
          }),
        })
      )
    ).toBe(true);

    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(runtime.state).toBe('kernel');
    expect(order).toEqual(['core', 'integration', 'after-commit', 'queued']);
    expect(target).toMatchObject({ version: '1.0.0', releaseId: RELEASE });
    expect(Object.isFrozen(target.que)).toBe(true);
    expect(Object.getOwnPropertyDescriptor(target, '_internal')).toMatchObject({
      enumerable: false,
      writable: false,
      configurable: false,
    });
    expect(
      (target as { _registerIntegration?: (value: unknown) => boolean })._registerIntegration?.({
        id: 'late',
      })
    ).toBe(false);
  });

  it('publishes the exact closure-supplied immutable boot snapshot without rereading the target', async () => {
    const target: Record<string, unknown> = {};
    const acceptedBoot = snapshotTsjsBootV1(
      {
        abi: 1,
        releaseId: RELEASE,
        manifest: manifest(['test_module']),
        ...boot(),
        integrations: { version: 1, entries: [] },
      },
      RELEASE
    );
    expect(acceptedBoot).toBeDefined();
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: acceptedBoot!.manifest,
      knownIntegrationIds: Object.freeze(['test_module']),
      catalog: Object.freeze([
        Object.freeze({
          id: 'test_module',
          phase: 'takeover' as const,
          trigger: null,
          config: null,
          consumes: Object.freeze([]),
          provides: Object.freeze([]),
        }),
      ]),
      boot: acceptedBoot,
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });

    expect(runtime.start()).toBe(true);
    target.boot = Object.freeze({ publisherReplacement: true });
    expect(
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'test_module',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: ({ config }: { config: unknown }) => {
            expect(config).toBeUndefined();
            return Object.freeze({ activate: () => undefined });
          },
        })
      )
    ).toBe(true);
    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(target.boot).toBe(acceptedBoot);
  });

  it('publishes direct.v1 through stable public closures only after provider activation', async () => {
    const target: Record<string, unknown> = {};
    const addAdUnits = vi.fn((candidate: unknown) => Object.freeze({ candidate }));
    const requestAds = vi.fn(async (_candidate?: unknown) =>
      Object.freeze({ slots: Object.freeze([]) })
    );
    let active = false;
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['render_runtime']),
      knownIntegrationIds: Object.freeze(['render_runtime']),
      catalog: Object.freeze([
        Object.freeze({
          id: 'render_runtime',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze(['runtime.v1']),
          provides: Object.freeze(['direct.v1']),
        }),
      ]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    expect(runtime.start()).toBe(true);
    expect(
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'render_runtime',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: ({ interfaces }: { interfaces: Readonly<Record<string, unknown>> }) => {
            expect(Reflect.ownKeys(interfaces)).toEqual(['runtime.v1']);
            return Object.freeze({
              activate: ({ onDispose }: { onDispose(callback: () => void): void }) => {
                active = true;
                onDispose(() => {
                  active = false;
                });
              },
              interfaces: Object.freeze({
                'direct.v1': Object.freeze({
                  addAdUnits: (candidate: unknown) => {
                    if (!active) throw new Error('inactive');
                    return addAdUnits(candidate);
                  },
                  requestAds: async (candidate?: unknown) => {
                    if (!active) throw new Error('inactive');
                    return requestAds(candidate);
                  },
                  diagnostics: Object.freeze({ owner: 'render_runtime' }),
                }),
              }),
            });
          },
        })
      )
    ).toBe(true);

    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    const api = target as {
      addAdUnits: (candidate: unknown) => unknown;
      requestAds: (candidate?: unknown) => Promise<unknown>;
      diagnostics: unknown;
    };
    expect(api.addAdUnits('unit')).toEqual({ candidate: 'unit' });
    await expect(api.requestAds()).resolves.toEqual({ slots: [] });
    expect(api.diagnostics).toEqual({ owner: 'render_runtime' });
    runtime.dispose();
    expect(() => api.addAdUnits('late')).toThrow('inactive');
  });

  it('publishes the staged takeover GPT diagnostics API without waiting for presentation', async () => {
    const target: Record<string, unknown> = {};
    const renderTrace = Object.freeze({ current: vi.fn(), history: vi.fn(), subscribe: vi.fn() });
    const gpt = Object.freeze({
      snapshot: vi.fn(() => Object.freeze({ slots: Object.freeze([]) })),
      export: vi.fn(),
      subscribe: vi.fn(() => vi.fn()),
      show: vi.fn(),
      hide: vi.fn(),
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['render_runtime', 'gpt_diagnostics', 'diagnostics_presentation']),
      knownIntegrationIds: Object.freeze([
        'render_runtime',
        'gpt_diagnostics',
        'diagnostics_presentation',
      ]),
      catalog: Object.freeze([
        Object.freeze({
          id: 'render_runtime',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze(['runtime.v1']),
          provides: Object.freeze(['direct.v1']),
        }),
        Object.freeze({
          id: 'gpt_diagnostics',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze(['runtime.v1']),
          provides: Object.freeze(['gpt_diag.v1']),
        }),
        Object.freeze({
          id: 'diagnostics_presentation',
          phase: 'deferred' as const,
          trigger: 'first_display_or_idle' as const,
          consumes: Object.freeze(['runtime.v1', 'gpt_diag.v1']),
          provides: Object.freeze([]),
        }),
      ]),
      boot: {
        ...boot(),
        diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: true } },
      },
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    expect(runtime.start()).toBe(true);
    expect(
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'render_runtime',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: () =>
            Object.freeze({
              activate: () => undefined,
              interfaces: Object.freeze({
                'direct.v1': Object.freeze({
                  addAdUnits: vi.fn(),
                  requestAds: vi.fn(),
                  diagnostics: Object.freeze({ renderTrace }),
                }),
              }),
            }),
        })
      )
    ).toBe(true);
    expect(
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'gpt_diagnostics',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: () =>
            Object.freeze({
              activate: () => undefined,
              interfaces: Object.freeze({
                'gpt_diag.v1': Object.freeze({ api: gpt, attachPresentation: vi.fn() }),
              }),
            }),
        })
      )
    ).toBe(true);

    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    const diagnostics = target['diagnostics'] as Readonly<Record<string, unknown>>;
    expect(Object.isFrozen(diagnostics)).toBe(true);
    expect(Reflect.ownKeys(diagnostics)).toEqual(['renderTrace', 'gpt']);
    expect(diagnostics['renderTrace']).toBe(renderTrace);
    expect(diagnostics['gpt']).toBe(gpt);
  });

  it('binds creative and GPT diagnostics from the private validated boot snapshot', async () => {
    const target: Record<string, unknown> = {};
    const prepared = new Map<string, unknown>();
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['creative', 'gpt_diagnostics', 'diagnostics_presentation']),
      knownIntegrationIds: Object.freeze([
        'creative',
        'gpt_diagnostics',
        'diagnostics_presentation',
      ]),
      catalog: Object.freeze([
        Object.freeze({
          id: 'creative',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze(['runtime.v1']),
          provides: Object.freeze([]),
        }),
        Object.freeze({
          id: 'gpt_diagnostics',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze(['runtime.v1']),
          provides: Object.freeze(['gpt_diag.v1']),
        }),
        Object.freeze({
          id: 'diagnostics_presentation',
          phase: 'deferred' as const,
          trigger: 'first_display_or_idle' as const,
          consumes: Object.freeze(['runtime.v1', 'gpt_diag.v1']),
          provides: Object.freeze([]),
        }),
      ]),
      boot: {
        ...boot(),
        creative: { version: 1, enabled: true, clickGuard: true, renderGuard: false },
        diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: true } },
      },
      getBindings: () =>
        Object.freeze({
          config: Object.freeze({ publisherControlled: true }),
          interfaces: Object.freeze({}),
        }),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    expect(runtime.start()).toBe(true);
    for (const id of ['creative', 'gpt_diagnostics']) {
      expect(
        runtime.registerIntegration(
          takeoverRegistration({
            abi: 1,
            id,
            phase: 'takeover',
            releaseId: RELEASE,
            prepare: ({ config }: { config: unknown }) => {
              prepared.set(id, config);
              return id === 'gpt_diagnostics'
                ? Object.freeze({
                    activate: () => undefined,
                    interfaces: Object.freeze({
                      'gpt_diag.v1': Object.freeze({
                        api: Object.freeze({
                          snapshot: vi.fn(),
                          export: vi.fn(),
                          subscribe: vi.fn(),
                          show: vi.fn(),
                          hide: vi.fn(),
                        }),
                        attachPresentation: vi.fn(),
                      }),
                    }),
                  })
                : Object.freeze({ activate: () => undefined });
            },
          })
        )
      ).toBe(true);
    }

    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(prepared.get('creative')).toEqual({
      version: 1,
      enabled: true,
      clickGuard: true,
      renderGuard: false,
    });
    expect(prepared.get('gpt_diagnostics')).toEqual({ active: true });
    expect(Object.isFrozen(prepared.get('creative'))).toBe(true);
    expect(Object.isFrozen(prepared.get('gpt_diagnostics'))).toBe(true);
  });

  it('keeps the authenticated registrar live and starts deferred loading only after the gate', async () => {
    vi.useFakeTimers();
    const frames: FrameRequestCallback[] = [];
    const idle: Array<() => void> = [];
    const runtimeHash = 'c'.repeat(64);
    const deferredHash = 'd'.repeat(64);
    const runtimeScript = document.createElement('script');
    runtimeScript.id = 'trustedserver-js';
    runtimeScript.src = `${window.location.origin}/static/tsjs=tsjs-unified.min.js?v=${runtimeHash}`;
    document.head.insertBefore(runtimeScript, null);
    Object.defineProperty(document, 'currentScript', {
      configurable: true,
      value: runtimeScript,
    });
    const target: Record<string, unknown> = {};
    const deferredPrepare = vi.fn(() => Object.freeze({ activate: () => undefined }));
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      document,
      manifest: {
        version: 1,
        releaseId: RELEASE,
        firstDisplay: null,
        runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${runtimeHash}`,
        integrations: [
          { id: 'render_runtime', phase: 'takeover' },
          {
            id: 'gpt_later',
            phase: 'deferred',
            trigger: 'first_display_or_idle',
            src: `/static/tsjs=tsjs-gpt_later.min.js?v=${deferredHash}`,
          },
        ],
      },
      knownIntegrationIds: Object.freeze(['render_runtime', 'gpt_later']),
      catalog: Object.freeze([
        Object.freeze({
          id: 'render_runtime',
          phase: 'takeover' as const,
          trigger: null,
          consumes: Object.freeze([]),
          provides: Object.freeze([]),
        }),
        Object.freeze({
          id: 'gpt_later',
          phase: 'deferred' as const,
          trigger: 'first_display_or_idle' as const,
          consumes: Object.freeze([]),
          provides: Object.freeze([]),
        }),
      ]),
      boot: boot(),
      getBindings: () => Object.freeze({ config: undefined, interfaces: Object.freeze({}) }),
      phaseScheduler: {
        cancelAnimationFrame: vi.fn(),
        cancelIdleCallback: vi.fn(),
        clearTimeout,
        requestAnimationFrame: (callback) => {
          frames.push(callback);
          return frames.length;
        },
        requestIdleCallback: (callback) => {
          idle.push(callback);
          return idle.length;
        },
        setTimeout,
      },
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(true);
    expect(
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'render_runtime',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: () => Object.freeze({ activate: () => undefined }),
        })
      )
    ).toBe(true);
    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(Object.getOwnPropertyDescriptor(target, '_registerIntegration')).toMatchObject({
      configurable: false,
      enumerable: true,
      writable: false,
    });
    expect(document.head.querySelectorAll('script')).toHaveLength(1);

    expect(runtime.protectFirstDisplayAttemptBatch([Promise.resolve()])).toBe(true);
    await Promise.resolve();
    await Promise.resolve();
    frames.shift()?.(1);
    frames.shift()?.(2);
    idle.shift()?.();
    await Promise.resolve();
    await Promise.resolve();
    const deferredScript = [...document.head.querySelectorAll('script')].find(
      (script) => script !== runtimeScript
    );
    expect(deferredScript?.src).toContain('tsjs-gpt_later.min.js');
    Object.defineProperty(document, 'currentScript', {
      configurable: true,
      value: deferredScript,
    });
    const register = target['_registerIntegration'];
    expect(typeof register).toBe('function');
    expect(
      Reflect.apply(register as (...args: unknown[]) => unknown, target, [
        {
          abi: 1,
          id: 'gpt_later',
          phase: 'deferred',
          releaseId: RELEASE,
          prepare: deferredPrepare,
        },
      ])
    ).toBe(true);
    deferredScript?.dispatchEvent(new Event('load'));
    await vi.waitFor(() => expect(deferredPrepare).toHaveBeenCalledOnce());
  });

  it('loads overlay-only presentation, GPT later, and Prebid later as separate authenticated artifacts', async () => {
    vi.useFakeTimers();
    const runtimeHash = 'c'.repeat(64);
    const deferredHash = 'd'.repeat(64);
    const runtimeScript = document.createElement('script');
    runtimeScript.id = 'trustedserver-js';
    runtimeScript.src = `${window.location.origin}/static/tsjs=tsjs-unified.min.js?v=${runtimeHash}`;
    document.head.insertBefore(runtimeScript, null);
    let executingScript: HTMLScriptElement | null = runtimeScript;
    vi.spyOn(document, 'currentScript', 'get').mockImplementation(() => executingScript);
    const frames: FrameRequestCallback[] = [];
    const idle: Array<() => void> = [];
    const target: Record<string, unknown> = {};
    const traceAttach = vi.fn(() => vi.fn());
    const traceDiagnostics = Object.freeze({
      current: vi.fn(() => Object.freeze({})),
      history: vi.fn(() => Object.freeze([])),
      subscribe: vi.fn(() => vi.fn()),
    });
    const traceDataCapability = Object.freeze({ diagnostics: traceDiagnostics });
    const tracePresentationCapability = Object.freeze({ attachPresentation: traceAttach });
    const gptLaterRelease = vi.fn();
    const prebidLaterRelease = vi.fn();
    const gptConfig = { gamAttributionEnabled: true };
    const prebidConfig = { accountId: 'publisher' };
    const gptLater = Object.freeze({
      activate: vi.fn(() => gptLaterRelease),
      start: vi.fn(),
    });
    const prebidLater = Object.freeze({
      activate: vi.fn(() => prebidLaterRelease),
      start: vi.fn(),
    });
    const deferredIds = Object.freeze(['diagnostics_presentation', 'gpt_later', 'prebid_later']);
    const manifestEntries = deferredIds.map((id) =>
      Object.freeze({
        id,
        phase: 'deferred' as const,
        trigger: 'first_display_or_idle' as const,
        src: `/static/tsjs=tsjs-${id}.min.js?v=${deferredHash}`,
      })
    );
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      document,
      manifest: {
        version: 1,
        releaseId: RELEASE,
        firstDisplay: null,
        runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${runtimeHash}`,
        integrations: [{ id: 'trace_provider', phase: 'takeover' }, ...manifestEntries],
      },
      knownIntegrationIds: Object.freeze([
        'trace_provider',
        'optional_gpt_diag_provider',
        ...deferredIds,
      ]),
      catalog: Object.freeze([
        Object.freeze({
          id: 'trace_provider',
          phase: 'takeover' as const,
          trigger: null,
          config: null,
          consumes: Object.freeze([]),
          provides: Object.freeze(['trace.v1', 'trace.presentation.v1']),
        }),
        Object.freeze({
          id: 'optional_gpt_diag_provider',
          phase: 'takeover' as const,
          trigger: null,
          config: null,
          consumes: Object.freeze([]),
          provides: Object.freeze(['gpt_diag.v1']),
        }),
        Object.freeze({
          id: 'diagnostics_presentation',
          phase: 'deferred' as const,
          trigger: 'first_display_or_idle' as const,
          config: null,
          consumes: Object.freeze([
            'runtime.v1',
            'trace.presentation.v1',
            'gpt_diag.v1?gpt_diagnostics_active',
          ]),
          provides: Object.freeze([]),
        }),
        ...deferredIds.slice(1).map((id) =>
          Object.freeze({
            id,
            phase: 'deferred' as const,
            trigger: 'first_display_or_idle' as const,
            config: id === 'gpt_later' ? ('gpt' as const) : ('prebid' as const),
            consumes: Object.freeze([]),
            provides: Object.freeze([]),
          })
        ),
      ]),
      boot: {
        ...boot(),
        integrations: {
          version: 1,
          entries: [
            { id: 'gpt', config: gptConfig },
            { id: 'prebid', config: prebidConfig },
          ],
        },
        diagnostics: { version: 1, renderTraceOverlay: true, gpt: { active: false } },
      },
      getBindings: (id) =>
        Object.freeze({
          config: id === 'gpt_later' || id === 'prebid_later' ? Object.freeze({}) : undefined,
          interfaces: Object.freeze(
            id === 'gpt_later'
              ? { gpt_later: gptLater }
              : id === 'prebid_later'
                ? { prebid_later: prebidLater }
                : {}
          ),
        }),
      phaseScheduler: {
        cancelAnimationFrame: vi.fn(),
        cancelIdleCallback: vi.fn(),
        clearTimeout,
        requestAnimationFrame: (callback) => {
          frames.push(callback);
          return frames.length;
        },
        requestIdleCallback: (callback) => {
          idle.push(callback);
          return idle.length;
        },
        setTimeout,
      },
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(true);
    expect(
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'trace_provider',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: () =>
            Object.freeze({
              activate: () => undefined,
              interfaces: Object.freeze({
                'trace.v1': traceDataCapability,
                'trace.presentation.v1': tracePresentationCapability,
              }),
            }),
        })
      )
    ).toBe(true);
    expect(Reflect.ownKeys(traceDataCapability)).toEqual(['diagnostics']);
    expect(traceDataCapability).not.toHaveProperty('attachPresentation');
    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(document.head.querySelectorAll('script')).toHaveLength(1);
    expect(traceAttach).not.toHaveBeenCalled();
    expect(gptLater.activate).not.toHaveBeenCalled();
    expect(prebidLater.activate).not.toHaveBeenCalled();

    const loadedSources: string[] = [];
    const originalHeadAppend = document.head.append.bind(document.head);
    vi.spyOn(document.head, 'append').mockImplementation((...nodes) => {
      originalHeadAppend(...nodes);
      for (const node of nodes) {
        if (!(node instanceof HTMLScriptElement) || node === runtimeScript) continue;
        const entry = manifestEntries.find(({ src }) => node.src.endsWith(src));
        if (!entry) throw new Error('Unexpected deferred artifact source');
        executingScript = node;
        loadedSources.push(node.src);
        const registration =
          entry.id === 'diagnostics_presentation'
            ? createDiagnosticsPresentationIntegrationRegistration(RELEASE)
            : createLifecycleIntegrationRegistration(entry.id, RELEASE);
        expect(runtime.registerIntegration(registration)).toBe(true);
        node.onload?.(new Event('load'));
        executingScript = runtimeScript;
      }
    });

    expect(runtime.protectFirstDisplayAttemptBatch([Promise.resolve()])).toBe(true);
    await Promise.resolve();
    await Promise.resolve();
    frames.shift()?.(1);
    frames.shift()?.(2);
    idle.shift()?.();
    await vi.waitFor(() => {
      expect(traceAttach).toHaveBeenCalledOnce();
      expect(gptLater.start).toHaveBeenCalledOnce();
      expect(prebidLater.start).toHaveBeenCalledOnce();
    });
    expect(loadedSources).toEqual(
      manifestEntries.map(({ src }) => new URL(src, window.location.origin).href)
    );
    expect(new Set(loadedSources)).toHaveLength(3);
    expect(loadedSources.every((source) => !source.includes('tsjs-unified'))).toBe(true);
    expect(gptLater.activate).toHaveBeenCalledOnce();
    expect(prebidLater.activate).toHaveBeenCalledOnce();
    const acceptedCarrier = (
      target['boot'] as {
        integrations: { entries: readonly { id: string; config: unknown }[] };
      }
    ).integrations;
    const acceptedGpt = acceptedCarrier.entries.find(({ id }) => id === 'gpt')?.config;
    const acceptedPrebid = acceptedCarrier.entries.find(({ id }) => id === 'prebid')?.config;
    expect(gptLater.activate).toHaveBeenCalledWith(acceptedGpt);
    expect(gptLater.start).toHaveBeenCalledWith(acceptedGpt);
    expect(prebidLater.activate).toHaveBeenCalledWith(acceptedPrebid);
    expect(prebidLater.start).toHaveBeenCalledWith(acceptedPrebid);
    expect(acceptedGpt).not.toBe(gptConfig);
    expect(acceptedPrebid).not.toBe(prebidConfig);

    runtime.dispose();
    expect(gptLaterRelease).toHaveBeenCalledOnce();
    expect(prebidLaterRelease).toHaveBeenCalledOnce();
  });

  it('resolves the frozen diagnostics namespace only after core and module activation', async () => {
    const target: Record<string, unknown> = {};
    const diagnostics = Object.freeze({ renderTrace: Object.freeze({}) });
    let activated = false;
    const getDiagnosticsForPublish = vi.fn(() => {
      expect(activated).toBe(true);
      return diagnostics;
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      activateCore: () => {
        activated = true;
      },
      getDiagnosticsForPublish,
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({ premature: true }),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(true);
    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(getDiagnosticsForPublish).toHaveBeenCalledOnce();
    expect(target['diagnostics']).toBe(diagnostics);
  });

  it('prepares inert owner interfaces before module preparation and activates afterward', async () => {
    const order: string[] = [];
    let prepared = false;
    const runtime = createRuntime({
      target: { que: [() => order.push('drain')] },
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      prepareOwner: ({ boot: acceptedBoot, onDispose }) => {
        expect(Object.isFrozen(acceptedBoot)).toBe(true);
        prepared = true;
        order.push('owner:prepare');
        onDispose(() => order.push('owner:dispose'));
      },
      getBindings: () => {
        expect(prepared).toBe(true);
        order.push('bindings');
        return { config: Object.freeze({}), interfaces: Object.freeze({}) };
      },
      activateOwner: () => order.push('owner:activate'),
      activateCore: () => order.push('core:activate'),
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(true);
    expect(
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'gpt',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: ({ onDispose }: { onDispose(callback: () => void): void }) => {
            order.push('module:prepare');
            onDispose(() => order.push('module:dispose'));
            return {
              activate: ({ afterCommit }: { afterCommit(callback: () => void): void }) => {
                order.push('module:activate');
                afterCommit(() => order.push('after-commit'));
              },
            };
          },
        })
      )
    ).toBe(true);

    const result = await runtime.install();
    expect(result).toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'owner:prepare',
      'bindings',
      'module:prepare',
      'owner:activate',
      'core:activate',
      'module:activate',
      'after-commit',
      'drain',
    ]);

    if (result.state === 'kernel') result.dispose();
    expect(order.slice(-2)).toEqual(['module:dispose', 'owner:dispose']);
  });

  it('keeps persistent activation and publication inside the supplied takeover call stack', async () => {
    const order: string[] = [];
    const runtime = createRuntime({
      target: { que: [() => order.push('drain')] },
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      prepareOwner: () => order.push('prepare'),
      activateOwner: () => order.push('activate'),
      coordinateTakeover: (prepared) => {
        order.push('takeover:begin');
        const candidate = takeoverHandoff();
        const handoff = prepared.validateHandoff(candidate.handoff, candidate.outline);
        expect(handoff).toBeDefined();
        prepared.activate(
          Object.freeze({
            version: 1 as const,
            adoptInitialDisplay: true as const,
            handoff: handoff!,
            identities: Object.freeze([]),
          })
        );
        order.push('takeover:activated');
        prepared.commit();
        order.push('takeover:committed');
      },
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(true);
    const result = await runtime.install();
    expect(result, JSON.stringify(order)).toMatchObject({ state: 'kernel' });
    expect(order).toEqual([
      'prepare',
      'takeover:begin',
      'activate',
      'takeover:activated',
      'drain',
      'takeover:committed',
    ]);
  });

  it('stops activation when owner activation disposes the installing runtime', async () => {
    const activateCore = vi.fn();
    const activateModule = vi.fn();
    const disposeOwner = vi.fn();
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      activateOwner: ({ onDispose }) => {
        onDispose(disposeOwner);
        runtime.dispose();
      },
      activateCore,
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(true);
    expect(
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'gpt',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: () => ({ activate: activateModule }),
        })
      )
    ).toBe(true);

    await expect(runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(activateCore).not.toHaveBeenCalled();
    expect(activateModule).not.toHaveBeenCalled();
    expect(disposeOwner).toHaveBeenCalledOnce();
    expect(runtime.state).toBe('fallback');
    expect(target).toMatchObject({
      _internal: { state: 'fallback', releaseId: RELEASE, reason: 'bundle_partial' },
    });
  });

  it('runs queued work at the exact activation, commit, afterCommit, and FIFO drain boundaries', async () => {
    const order: string[] = [];
    let commitPushInstalled = false;
    const backing: { que?: unknown[]; version?: string } = {
      que: [
        function (this: unknown) {
          expect(this).toBe(target);
          order.push('preload-start');
          target.que?.push(() => order.push('preload-nested'));
          order.push('preload-end');
        },
      ],
    };
    const target = new Proxy(backing, {
      defineProperty(object, key, descriptor) {
        if (key === 'version' && !commitPushInstalled) {
          commitPushInstalled = true;
          object.que?.push(() => order.push('commit-enqueued'));
        }
        return Reflect.defineProperty(object, key, descriptor);
      },
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      activateCore: () => {
        order.push('core-activation');
        target.que?.push(() => order.push('core-enqueued'));
      },
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(true);
    expect(
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'gpt',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: () => ({
            activate: ({ afterCommit }: { afterCommit(callback: () => void): void }) => {
              order.push('module-activation');
              target.que?.push(() => order.push('module-enqueued'));
              afterCommit(() => {
                order.push('after-commit-start');
                target.que?.push(() => order.push('after-commit-enqueued'));
                order.push('after-commit-end');
              });
            },
          }),
        })
      )
    ).toBe(true);

    await expect(runtime.install()).resolves.toMatchObject({ state: 'kernel' });

    expect(order).toEqual([
      'core-activation',
      'module-activation',
      'commit-enqueued',
      'after-commit-start',
      'after-commit-enqueued',
      'after-commit-end',
      'preload-start',
      'preload-nested',
      'preload-end',
      'core-enqueued',
      'module-enqueued',
    ]);
  });

  it.each([
    ['invalid manifest', { version: 2 }, 'abi_mismatch'],
    ['missing bundle', manifest(['gpt']), 'bundle_partial'],
  ] as const)('commits terminal fallback for %s', async (_name, candidateManifest, reason) => {
    vi.useFakeTimers();
    const queued = vi.fn();
    const activateCore = vi.fn();
    const target = { que: [queued], boot: boot() };
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: candidateManifest,
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: target.boot,
      activateCore,
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    runtime.start();
    const installed = runtime.install();
    if (reason === 'bundle_partial') await vi.advanceTimersByTimeAsync(10_000);
    await expect(installed).resolves.toEqual({ state: 'fallback', reason });

    expect(runtime.state).toBe('fallback');
    expect(activateCore).not.toHaveBeenCalled();
    expect(queued).toHaveBeenCalledOnce();
    expect(target).toMatchObject({ version: '1.0.0', releaseId: RELEASE });
    expect((target as { _internal?: unknown })._internal).toEqual({
      state: 'fallback',
      releaseId: RELEASE,
      reason,
    });
    await expect(
      (target as unknown as { requestAds(options?: unknown): Promise<unknown> }).requestAds()
    ).resolves.toEqual({ slots: [] });
    expect(
      (target as unknown as { _registerIntegration(value: unknown): boolean })._registerIntegration(
        {
          id: 'gpt',
          releaseId: RELEASE,
          prepare: vi.fn(),
        }
      )
    ).toBe(false);
  });

  it('publishes the captured exact takeover source when the manifest field is missing', async () => {
    const runtimeSrc = `/static/tsjs=tsjs-unified.min.js?v=${'d'.repeat(64)}`;
    const runtimeScript = document.createElement('script');
    runtimeScript.id = 'trustedserver-js';
    runtimeScript.src = new URL(runtimeSrc, window.location.origin).href;
    document.head.insertBefore(runtimeScript, null);
    Object.defineProperty(document, 'currentScript', {
      configurable: true,
      value: runtimeScript,
    });
    const candidateManifest = {
      version: 1,
      releaseId: RELEASE,
      integrations: [],
    };
    const target = {
      boot: {
        ...boot(),
        manifest: candidateManifest,
      },
    };
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      document,
      manifest: candidateManifest,
      knownIntegrationIds: Object.freeze([] as string[]),
      boot: target.boot,
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(true);
    await expect(runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
    expect((target as { boot: { manifest: unknown } }).boot.manifest).toEqual({
      version: 1,
      releaseId: RELEASE,
      firstDisplay: null,
      runtimeSrc,
      integrations: [],
    });
  });

  it('publishes the captured exact takeover source when the manifest field is malformed', async () => {
    const runtimeSrc = `/static/tsjs=tsjs-unified.min.js?v=${'d'.repeat(64)}`;
    const runtimeScript = document.createElement('script');
    runtimeScript.id = 'trustedserver-js';
    runtimeScript.src = new URL(runtimeSrc, window.location.origin).href;
    document.head.insertBefore(runtimeScript, null);
    Object.defineProperty(document, 'currentScript', {
      configurable: true,
      value: runtimeScript,
    });
    const candidateManifest = {
      version: 1,
      releaseId: RELEASE,
      firstDisplay: null,
      runtimeSrc: `${runtimeSrc}&publisher=1`,
      integrations: [],
    };
    const target = {
      boot: {
        ...boot(),
        manifest: candidateManifest,
      },
    };
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      document,
      manifest: candidateManifest,
      knownIntegrationIds: Object.freeze([] as string[]),
      boot: target.boot,
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(true);
    await expect(runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
    expect((target as { boot: { manifest: unknown } }).boot.manifest).toEqual({
      version: 1,
      releaseId: RELEASE,
      firstDisplay: null,
      runtimeSrc,
      integrations: [],
    });
  });

  it('leaves the namespace unclaimed when no trusted takeover source exists', () => {
    const target = {
      boot: boot(),
      que: [vi.fn()],
    };
    const bootDescriptor = Object.getOwnPropertyDescriptor(target, 'boot');
    const queueDescriptor = Object.getOwnPropertyDescriptor(target, 'que');
    const runtime = createRuntimeOwner({
      target,
      releaseId: RELEASE,
      manifest: { version: 1, releaseId: RELEASE, integrations: [] },
      knownIntegrationIds: Object.freeze([] as string[]),
      boot: target.boot,
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(runtime.start()).toBe(false);
    expect(runtime.state).toBe('unclaimed');
    expect(Object.getOwnPropertyDescriptor(target, 'boot')).toEqual(bootDescriptor);
    expect(Object.getOwnPropertyDescriptor(target, 'que')).toEqual(queueDescriptor);
    expect(target).not.toHaveProperty('_registerIntegration');
    expect(target).not.toHaveProperty('_internal');
  });

  it('publishes an exact terminal namespace with no publisher-owned fields', async () => {
    const target = {
      que: [] as unknown[],
      diagnostics: { legacy: true },
      adInit: vi.fn(),
      renderAdUnit: vi.fn(),
      setConfig: vi.fn(),
      publisher: { retained: true },
    };
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });

    expect(runtime.start()).toBe(true);
    await expect(runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'abi_mismatch',
    });

    expect(Object.prototype.hasOwnProperty.call(target, 'diagnostics')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(target, 'adInit')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(target, 'renderAdUnit')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(target, 'setConfig')).toBe(false);
    expect(target).not.toHaveProperty('publisher');
  });

  it('allows exactly one bootstrap owner for a namespace', () => {
    const target = {};
    const options = {
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([] as string[]),
      boot: boot(),
      kernel: {
        addAdUnits: () => ({ registered: [] }),
        diagnostics: Object.freeze({}),
        requestAds: async () => ({ slots: [] }),
      },
    };
    const first = createRuntime(options);
    const second = createRuntime(options);

    expect(first.start()).toBe(true);
    expect(second.start()).toBe(false);
    expect(first.generation).not.toBe(second.generation);
  });

  it('rejects a detached async no-agent preparation before activation', async () => {
    const target = {};
    const staleCoreActivation = vi.fn();
    const staleModuleActivation = vi.fn();
    const staleDisposal = vi.fn();
    let resolveStalePreparation: (() => void) | undefined;
    const stalePreparation = new Promise<void>((resolve) => {
      resolveStalePreparation = resolve;
    });
    const first = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      activateCore: staleCoreActivation,
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });
    const second = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });

    expect(first.start()).toBe(true);
    expect(
      first.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'gpt',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: ({ onDispose }: { onDispose(callback: () => void): void }) => {
            onDispose(staleDisposal);
            return stalePreparation.then(() => ({ activate: staleModuleActivation }));
          },
        })
      )
    ).toBe(true);
    const staleInstall = first.install();
    expect(first.state).toBe('fallback');
    expect(Reflect.deleteProperty(target, '_registerIntegration')).toBe(false);
    expect(second.start()).toBe(false);
    resolveStalePreparation?.();
    await expect(staleInstall).resolves.toEqual({ state: 'fallback', reason: 'bundle_partial' });

    expect(staleCoreActivation).not.toHaveBeenCalled();
    expect(staleModuleActivation).not.toHaveBeenCalled();
    expect(staleDisposal).toHaveBeenCalledOnce();
    expect(first.state).toBe('fallback');
    expect(second.state).toBe('unclaimed');
    expect((target as { _internal?: unknown })._internal).toEqual({
      state: 'fallback',
      releaseId: RELEASE,
      reason: 'bundle_partial',
    });
  });

  it('rejects registration when candidate reflection replaces the owner handshake', async () => {
    const target = {};
    const staleCoreActivation = vi.fn();
    const staleModuleActivation = vi.fn();
    const options = {
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    };
    const first = createRuntime({ ...options, activateCore: staleCoreActivation });
    const second = createRuntime(options);

    expect(first.start()).toBe(true);
    const registration = new Proxy(
      {
        abi: 1,
        id: 'gpt',
        phase: 'takeover',
        releaseId: RELEASE,
        prepareSync: () => ({ activate: staleModuleActivation }),
        prepare: () => ({ activate: staleModuleActivation }),
      },
      {
        ownKeys(candidate) {
          expect(Reflect.deleteProperty(target, '_registerIntegration')).toBe(true);
          return Reflect.ownKeys(candidate);
        },
      }
    );

    expect(first.registerIntegration(registration)).toBe(false);
    expect(second.start()).toBe(true);
    expect(
      second.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'gpt',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare: () => ({ activate: vi.fn() }),
        })
      )
    ).toBe(true);
    await expect(second.install()).resolves.toMatchObject({ state: 'kernel' });
    await expect(first.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });

    expect(staleCoreActivation).not.toHaveBeenCalled();
    expect(staleModuleActivation).not.toHaveBeenCalled();
    expect(first.state).toBe('failed');
    expect(second.state).toBe('kernel');
  });

  it('allows exactly one bootstrap owner across independently evaluated core modules', async () => {
    const target = {};
    const options = {
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([] as string[]),
      boot: boot(),
      kernel: {
        addAdUnits: () => ({ registered: [] }),
        diagnostics: Object.freeze({}),
        requestAds: async () => ({ slots: [] }),
      },
    };
    const firstModule = await import('../../src/kernel/runtime');
    vi.resetModules();
    const secondModule = await import('../../src/kernel/runtime');
    installTestRuntimeScript(document);
    const first = firstModule.createRuntime(options);
    const second = secondModule.createRuntime(options);

    expect(first.start()).toBe(true);
    expect(second.start()).toBe(false);
    expect(first.generation).not.toBe(second.generation);
  });

  it('refuses a conflicting terminal namespace before constructing an installing generation', () => {
    const target: { que: unknown[]; version?: string } = { que: [] };
    Object.defineProperty(target, 'version', {
      configurable: false,
      enumerable: true,
      value: 'publisher',
      writable: false,
    });
    const queueDescriptor = Object.getOwnPropertyDescriptor(target, 'que');
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([] as string[]),
      boot: boot(),
      kernel: {
        addAdUnits: () => ({ registered: [] }),
        diagnostics: Object.freeze({}),
        requestAds: async () => ({ slots: [] }),
      },
    });

    expect(runtime.start()).toBe(false);
    expect(runtime.state).toBe('unclaimed');
    expect(Object.getOwnPropertyDescriptor(target, 'que')).toEqual(queueDescriptor);
    expect(Reflect.ownKeys(target)).toEqual(['que', 'version']);
  });

  it.each([
    [
      'wrong release',
      {
        abi: 1,
        id: 'gpt',
        phase: 'takeover',
        releaseId: 'b'.repeat(64),
        prepareSync: vi.fn(),
        prepare: vi.fn(),
      },
    ],
    [
      'unknown id',
      {
        abi: 1,
        id: 'aps',
        phase: 'takeover',
        releaseId: RELEASE,
        prepareSync: vi.fn(),
        prepare: vi.fn(),
      },
    ],
  ])(
    'classifies %s registration as abi_mismatch without invoking module code',
    async (_name, registration) => {
      const runtime = createRuntime({
        target: {},
        releaseId: RELEASE,
        manifest: manifest(['gpt']),
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: boot(),
        kernel: {
          addAdUnits: vi.fn(),
          diagnostics: Object.freeze({}),
          requestAds: vi.fn(),
        },
      });
      runtime.start();

      expect(runtime.registerIntegration(registration)).toBe(false);
      await expect(runtime.install()).resolves.toEqual({
        state: 'fallback',
        reason: 'abi_mismatch',
      });
      expect(registration.prepare).not.toHaveBeenCalled();
    }
  );

  it('classifies duplicate registration as abi_mismatch', async () => {
    const prepare = vi.fn(() => ({ activate: vi.fn() }));
    const registration = takeoverRegistration({
      abi: 1,
      id: 'gpt',
      phase: 'takeover',
      releaseId: RELEASE,
      prepare,
    });
    const runtime = createRuntime({
      target: {},
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      kernel: {
        addAdUnits: vi.fn(),
        diagnostics: Object.freeze({}),
        requestAds: vi.fn(),
      },
    });
    runtime.start();
    expect(runtime.registerIntegration(registration)).toBe(true);
    expect(runtime.registerIntegration(registration)).toBe(false);

    await expect(runtime.install()).resolves.toEqual({ state: 'fallback', reason: 'abi_mismatch' });
    expect(prepare).not.toHaveBeenCalled();
  });

  it.each(['prepare_throw', 'prepare_reject', 'activate_throw'] as const)(
    'unwinds %s as bundle_partial',
    async (checkpoint) => {
      const disposed = vi.fn();
      const prepare =
        checkpoint === 'prepare_throw'
          ? () => {
              throw new Error('prepare');
            }
          : checkpoint === 'prepare_reject'
            ? async ({ onDispose }: { onDispose(callback: () => void): void }) => {
                onDispose(disposed);
                throw new Error('prepare');
              }
            : ({ onDispose }: { onDispose(callback: () => void): void }) => {
                onDispose(disposed);
                return {
                  activate: () => {
                    throw new Error('activate');
                  },
                };
              };
      const runtime = createRuntime({
        target: {},
        releaseId: RELEASE,
        manifest: manifest(['gpt']),
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: boot(),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      runtime.start();
      runtime.registerIntegration(
        takeoverRegistration({
          abi: 1,
          id: 'gpt',
          phase: 'takeover',
          releaseId: RELEASE,
          prepare,
        })
      );

      await expect(runtime.install()).resolves.toEqual({
        state: 'fallback',
        reason: 'bundle_partial',
      });
      if (checkpoint !== 'prepare_throw') expect(disposed).toHaveBeenCalledOnce();
    }
  );

  it('shares the ten-second watchdog with a hung preparation and ignores its late continuation', async () => {
    vi.useFakeTimers();
    let finish: ((value: { activate(): void }) => void) | undefined;
    const lateActivate = vi.fn();
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    runtime.registerIntegration(
      takeoverRegistration({
        abi: 1,
        id: 'gpt',
        phase: 'takeover',
        releaseId: RELEASE,
        prepare: () =>
          new Promise<{ activate(): void }>((resolve) => {
            finish = resolve;
          }),
      })
    );
    const installed = runtime.install();

    await vi.advanceTimersByTimeAsync(10_000);
    await expect(installed).resolves.toEqual({ state: 'fallback', reason: 'bundle_partial' });
    finish?.({ activate: lateActivate });
    await Promise.resolve();
    expect(lateActivate).not.toHaveBeenCalled();
    expect(runtime.state).toBe('fallback');
  });

  it('isolates afterCommit failure after kernel publication', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest(['gpt']),
      knownIntegrationIds: Object.freeze(['gpt']),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    runtime.registerIntegration(
      takeoverRegistration({
        abi: 1,
        id: 'gpt',
        phase: 'takeover',
        releaseId: RELEASE,
        prepare: () => ({
          activate: ({ afterCommit }: { afterCommit(callback: () => void): void }) =>
            afterCommit(() => {
              throw new Error('post commit');
            }),
        }),
      })
    );

    await expect(runtime.install()).resolves.toMatchObject({
      state: 'kernel',
      runtimeFailures: [{ id: 'gpt', phase: 'after_commit' }],
    });
    expect(runtime.state).toBe('kernel');
  });

  it('validates fallback calls and settles known, unknown, and aborted slots', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot([{ slot: 'known', outcome: 'no_bid' }]),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const api = target as unknown as {
      addAdUnits(units: unknown): unknown;
      requestAds(options?: unknown): Promise<unknown>;
      boot: unknown;
    };

    await expect(api.requestAds({ slots: ['known', 'unknown'] })).resolves.toEqual({
      slots: [
        { slot: 'known', path: 'primary', outcome: 'failed', reason: 'abi_mismatch' },
        { slot: 'unknown', path: 'primary', outcome: 'failed', reason: 'slot_unresolved' },
      ],
    });
    const controller = new AbortController();
    controller.abort();
    await expect(api.requestAds({ slots: ['known'], signal: controller.signal })).resolves.toEqual({
      slots: [{ slot: 'known', path: 'primary', outcome: 'cancelled', reason: 'caller_aborted' }],
    });
    await expect(api.requestAds({ slots: [] })).rejects.toBeInstanceOf(RequestAdsInputError);
    expect(() => api.addAdUnits({ code: '', mediaTypes: {} })).toThrow(AdUnitRegistrationError);
    expect(() =>
      api.addAdUnits({
        code: 'programmatic',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      })
    ).toThrow(TsjsUnavailableError);
    expect(Object.isFrozen(api.boot)).toBe(true);
  });

  it('substitutes the exact safe auction projection when boot data is hostile', async () => {
    const getter = vi.fn(() => ({ version: 1 }));
    const hostile = {};
    Object.defineProperty(hostile, 'auctionProjection', { enumerable: true, get: getter });
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: hostile,
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();

    expect(getter).not.toHaveBeenCalled();
    expect(
      (target as unknown as { boot: { auctionProjection: unknown } }).boot.auctionProjection
    ).toEqual({
      version: 1,
      auction: { version: 1, auctionId: 'fallback', results: [] },
      slots: [],
      bids: [],
    });
  });

  it('snapshots fallback boot before publisher mutation during installation', async () => {
    const target = { boot: boot([{ slot: 'initial', outcome: 'no_bid' }]) };
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: target.boot,
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    expect(runtime.start()).toBe(true);
    target.boot = boot([{ slot: 'mutated', outcome: 'no_bid' }]);

    await runtime.install();
    const api = target as unknown as {
      boot: { auctionProjection: { auction: { results: readonly { slot: string }[] } } };
      requestAds(options: unknown): Promise<unknown>;
    };
    expect(api.boot.auctionProjection.auction.results).toEqual([
      { slot: 'initial', outcome: 'no_bid' },
    ]);
    await expect(api.requestAds({ slots: ['initial', 'mutated'] })).resolves.toEqual({
      slots: [
        { slot: 'initial', path: 'primary', outcome: 'failed', reason: 'abi_mismatch' },
        { slot: 'mutated', path: 'primary', outcome: 'failed', reason: 'slot_unresolved' },
      ],
    });
  });

  it.each(['getPrototypeOf', 'ownKeys', 'getOwnPropertyDescriptor'] as const)(
    'maps a hostile request options %s trap to invalid_options',
    async (trap) => {
      const target = {};
      const runtime = createRuntime({
        target,
        releaseId: RELEASE,
        manifest: { version: 2 },
        knownIntegrationIds: Object.freeze([]),
        boot: boot(),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      runtime.start();
      await runtime.install();
      const requestAds = (target as unknown as { requestAds(value: unknown): Promise<unknown> })
        .requestAds;
      const optionsTarget = trap === 'getOwnPropertyDescriptor' ? { slots: ['known'] } : {};

      await expect(requestAds(hostileRecord(trap, optionsTarget))).rejects.toMatchObject({
        code: 'invalid_options',
      });
    }
  );

  it.each(['getPrototypeOf', 'ownKeys', 'getOwnPropertyDescriptor'] as const)(
    'maps a hostile addAdUnits unit %s trap to invalid_unit',
    async (trap) => {
      const target = {};
      const runtime = createRuntime({
        target,
        releaseId: RELEASE,
        manifest: { version: 2 },
        knownIntegrationIds: Object.freeze([]),
        boot: boot(),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      runtime.start();
      await runtime.install();
      const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;
      const unit = hostileRecord(trap, {
        code: 'hostile',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      });

      expect(() => addAdUnits(unit)).toThrow(
        expect.objectContaining({ code: 'invalid_unit', unitIndex: 0 })
      );
    }
  );

  it('maps hostile outer addAdUnits Array reflection to invalid_units', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;
    const units = new Proxy([], {
      ownKeys() {
        throw new Error('hostile outer Array');
      },
    });

    const error = thrownBy(() => addAdUnits(units));
    expect(error).toMatchObject({ code: 'invalid_units' });
    expect(Object.prototype.hasOwnProperty.call(error, 'unitIndex')).toBe(false);
  });

  it('maps a revoked outer addAdUnits Array proxy to invalid_units', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const { proxy, revoke } = Proxy.revocable([], {});
    revoke();

    const error = thrownBy(() =>
      (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits(proxy)
    );
    expect(error).toMatchObject({ code: 'invalid_units' });
    expect(Object.prototype.hasOwnProperty.call(error, 'unitIndex')).toBe(false);
  });

  it.each(['getPrototypeOf', 'ownKeys', 'getOwnPropertyDescriptor'] as const)(
    'substitutes exact safe boot for a hostile boot %s trap',
    async (trap) => {
      const target = {};
      const runtime = createRuntime({
        target,
        releaseId: RELEASE,
        manifest: { version: 2 },
        knownIntegrationIds: Object.freeze([]),
        boot: hostileRecord(trap, trap === 'getOwnPropertyDescriptor' ? boot() : {}),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      expect(runtime.start()).toBe(true);

      await expect(runtime.install()).resolves.toEqual({
        state: 'fallback',
        reason: 'abi_mismatch',
      });
      expect(
        (target as unknown as { boot: { auctionProjection: unknown } }).boot.auctionProjection
      ).toEqual({
        version: 1,
        auction: { version: 1, auctionId: 'fallback', results: [] },
        slots: [],
        bids: [],
      });
    }
  );

  it('substitutes exact safe boot when nested boot contract proxies throw', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: {
        cachePolicy: hostileRecord('ownKeys'),
        auctionProjection: hostileRecord('getPrototypeOf'),
      },
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    expect(runtime.start()).toBe(true);

    await expect(runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
    expect(
      (target as unknown as { boot: { auctionProjection: unknown } }).boot.auctionProjection
    ).toEqual({
      version: 1,
      auction: { version: 1, auctionId: 'fallback', results: [] },
      slots: [],
      bids: [],
    });
  });

  it('rejects a full boot whose server manifest disagrees with the accepted bundle manifest', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: { abi: 1, releaseId: RELEASE, manifest: manifest(['gpt']), ...boot() },
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();

    await expect(runtime.install()).resolves.toEqual({ state: 'fallback', reason: 'abi_mismatch' });
  });

  it('binds validation and fallback publication to the embedded bundle release', async () => {
    const serverRelease = 'b'.repeat(64);
    const serverManifest = { version: 1, releaseId: serverRelease, integrations: [] };
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: serverRelease,
      manifest: serverManifest,
      knownIntegrationIds: Object.freeze([]),
      boot: {
        abi: 1,
        releaseId: serverRelease,
        manifest: serverManifest,
        ...boot(),
      },
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();

    await expect(runtime.install()).resolves.toEqual({ state: 'fallback', reason: 'abi_mismatch' });
    expect(target).toMatchObject({
      releaseId: RELEASE,
      boot: { releaseId: RELEASE, manifest: { releaseId: RELEASE } },
      _internal: { state: 'fallback', releaseId: RELEASE, reason: 'abi_mismatch' },
    });
  });

  it('does not invoke hostile Array iterators at fallback input boundaries', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot([{ slot: 'known', outcome: 'no_bid' }]),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const iterator = vi.fn();
    const slots = ['known'];
    Object.defineProperty(slots, Symbol.iterator, { value: iterator });

    await expect(
      (target as unknown as { requestAds(value: unknown): Promise<unknown> }).requestAds({ slots })
    ).rejects.toMatchObject({ code: 'invalid_slots' });
    expect(iterator).not.toHaveBeenCalled();
  });

  it('uses exact addAdUnits dimension and bidder validation before refusing valid input', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;

    expect(() =>
      addAdUnits({ code: 'zero', mediaTypes: { banner: { sizes: [[0, 250]] } } })
    ).toThrow(expect.objectContaining({ code: 'invalid_dimensions' }));
    expect(() =>
      addAdUnits({ code: 'large', mediaTypes: { banner: { sizes: [[4097, 250]] } } })
    ).toThrow(expect.objectContaining({ code: 'dimensions_out_of_range' }));
    expect(() =>
      addAdUnits({
        code: 'bad-bidder',
        mediaTypes: { banner: { sizes: [[1, 1]] } },
        bids: [{ bidder: 'x'.repeat(65) }],
      })
    ).toThrow(expect.objectContaining({ code: 'invalid_bidder' }));
    expect(() =>
      addAdUnits({
        code: 'valid',
        mediaTypes: { banner: { sizes: [[1, 4096]] } },
        bids: [{ bidder: 'aps', params: { placement: 'one' } }],
      })
    ).toThrow(TsjsUnavailableError);
  });

  it.each([
    ['high', '\ud800'],
    ['low', '\udc00'],
  ] as const)(
    'rejects a lone %s UTF-16 surrogate in a programmatic slot code',
    async (_kind, code) => {
      const target = {};
      const runtime = createRuntime({
        target,
        releaseId: RELEASE,
        manifest: { version: 2 },
        knownIntegrationIds: Object.freeze([]),
        boot: boot(),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      runtime.start();
      await runtime.install();
      const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;

      expect(() => addAdUnits({ code, mediaTypes: { banner: { sizes: [[300, 250]] } } })).toThrow(
        expect.objectContaining({ code: 'invalid_code', unitIndex: 0 })
      );
    }
  );

  it('applies fallback slot collision and combined registry capacity validation', async () => {
    const makeFallback = async (slots: readonly string[]) => {
      const target = {};
      const runtime = createRuntime({
        target,
        releaseId: RELEASE,
        manifest: { version: 2 },
        knownIntegrationIds: Object.freeze([]),
        boot: boot(slots.map((slot) => ({ slot, outcome: 'no_bid' }))),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      });
      runtime.start();
      await runtime.install();
      return (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;
    };
    const collision = await makeFallback(['server']);
    expect(() =>
      collision({ code: 'server', mediaTypes: { banner: { sizes: [[300, 250]] } } })
    ).toThrow(expect.objectContaining({ code: 'slot_collision', unitIndex: 0 }));

    const full = await makeFallback(Array.from({ length: 256 }, (_, index) => `slot-${index}`));
    const capacityError = thrownBy(() =>
      full({ code: 'overflow', mediaTypes: { banner: { sizes: [[300, 250]] } } })
    );
    expect(capacityError).toMatchObject({ code: 'registry_capacity' });
    expect(Object.prototype.hasOwnProperty.call(capacityError, 'unitIndex')).toBe(false);
  });

  it('reports aggregate request overflow before combined registry capacity', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(
        Array.from({ length: 256 }, (_, index) => ({
          slot: `server-${index}`,
          outcome: 'no_bid',
        }))
      ),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;

    expect(() =>
      addAdUnits({
        code: 'programmatic-overflow',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [{ bidder: 'aps', params: { payload: 'x'.repeat(256 * 1024) } }],
      })
    ).toThrow(expect.objectContaining({ code: 'request_body_too_large' }));
  });

  it('accepts contract-valid large collections and deep params before refusing availability', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;
    const sizes = Array.from({ length: 257 }, () => [1, 1]);
    const bids = Array.from({ length: 257 }, () => ({ bidder: 'aps' }));
    const paramsArray = Array.from({ length: 4097 }, () => 0);
    let deepParams: object = { leaf: true };
    for (let depth = 0; depth < 128; depth += 1) deepParams = { child: deepParams };

    for (const unit of [
      { code: 'many-sizes', mediaTypes: { banner: { sizes } } },
      { code: 'many-bids', mediaTypes: { banner: { sizes: [[1, 1]] } }, bids },
      {
        code: 'large-params-array',
        mediaTypes: { banner: { sizes: [[1, 1]] } },
        bids: [{ bidder: 'aps', params: { values: paramsArray } }],
      },
      {
        code: 'deep-params',
        mediaTypes: { banner: { sizes: [[1, 1]] } },
        bids: [{ bidder: 'aps', params: deepParams }],
      },
    ]) {
      expect(() => addAdUnits(unit)).toThrow(TsjsUnavailableError);
    }
  });

  it('classifies an empty banner size list as invalid_media_types', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();

    expect(() =>
      (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits({
        code: 'empty-sizes',
        mediaTypes: { banner: { sizes: [] } },
      })
    ).toThrow(expect.objectContaining({ code: 'invalid_media_types', unitIndex: 0 }));
  });

  it('bounds an exponentially expanded shared params DAG without revisiting nodes', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const addAdUnits = (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits;
    const descriptorReads: number[] = [];
    let shared: object = { value: 'leaf' };
    for (let depth = 0; depth < 24; depth += 1) {
      const node = { left: shared, right: shared };
      const nodeIndex = descriptorReads.length;
      descriptorReads.push(0);
      shared = new Proxy(node, {
        getOwnPropertyDescriptor(object, key) {
          descriptorReads[nodeIndex] = (descriptorReads[nodeIndex] ?? 0) + 1;
          if ((descriptorReads[nodeIndex] ?? 0) > Reflect.ownKeys(object).length) {
            throw new Error('shared DAG node was expanded more than once');
          }
          return Reflect.getOwnPropertyDescriptor(object, key);
        },
      });
    }

    expect(() =>
      addAdUnits({
        code: 'shared-dag',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [{ bidder: 'aps', params: shared }],
      })
    ).toThrow(expect.objectContaining({ code: 'request_body_too_large' }));
    expect(descriptorReads.every((reads) => reads <= 2)).toBe(true);
  });

  it('measures addAdUnits input without invoking inherited toJSON hooks', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const hook = vi.fn(() => {
      throw new Error('publisher toJSON');
    });
    Object.defineProperty(Object.prototype, 'toJSON', { configurable: true, value: hook });
    Object.defineProperty(Array.prototype, 'toJSON', { configurable: true, value: hook });
    try {
      expect(() =>
        (target as unknown as { addAdUnits(value: unknown): unknown }).addAdUnits({
          code: 'valid',
          mediaTypes: { banner: { sizes: [[300, 250]] } },
          bids: [{ bidder: 'aps', params: { placement: 'one' } }],
        })
      ).toThrow(TsjsUnavailableError);
      expect(hook).not.toHaveBeenCalled();
    } finally {
      Reflect.deleteProperty(Object.prototype, 'toJSON');
      Reflect.deleteProperty(Array.prototype, 'toJSON');
    }
  });

  it('publishes an immutable exact logger facade', async () => {
    const target = {};
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: { version: 2 },
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    runtime.start();
    await runtime.install();
    const publicLog = (target as unknown as { log: object }).log;

    expect(Object.isFrozen(publicLog)).toBe(true);
    expect(Object.keys(publicLog)).toEqual([
      'setLevel',
      'getLevel',
      'error',
      'warn',
      'info',
      'debug',
    ]);
    expect(Reflect.set(publicLog, 'warn', vi.fn())).toBe(false);
  });

  it('returns false when queue descriptor reflection becomes hostile after preflight', () => {
    let queueDescriptorReads = 0;
    const backing = {};
    const target = new Proxy(backing, {
      getOwnPropertyDescriptor(object, key) {
        if (key === 'que') {
          queueDescriptorReads += 1;
          if (queueDescriptorReads === 2) throw new Error('hostile second queue reflection');
        }
        return Reflect.getOwnPropertyDescriptor(object, key);
      },
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    let started: boolean | undefined;

    expect(() => {
      started = runtime.start();
    }).not.toThrow();
    expect(started).toBe(false);
    expect(runtime.state).toBe('unclaimed');
    expect(queueDescriptorReads).toBe(2);
    expect(Object.prototype.hasOwnProperty.call(backing, '_registerIntegration')).toBe(false);
  });

  it('returns false when a claim mutation and its rollback restoration both throw', () => {
    const ingress: unknown[] = [];
    const backing = { que: ingress, boot: boot() };
    let queueDefinitionCalls = 0;
    const target = new Proxy(backing, {
      defineProperty(object, key, descriptor) {
        if (key === 'que') {
          queueDefinitionCalls += 1;
          if (queueDefinitionCalls === 1) {
            Reflect.defineProperty(object, key, descriptor);
            throw new Error('hostile claim definition');
          }
          if (queueDefinitionCalls === 2) throw new Error('hostile rollback definition');
        }
        return Reflect.defineProperty(object, key, descriptor);
      },
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: backing.boot,
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    let started: boolean | undefined;

    expect(() => {
      started = runtime.start();
    }).not.toThrow();
    expect(started).toBe(false);
    expect(runtime.state).toBe('unclaimed');
    expect(queueDefinitionCalls).toBe(2);
    expect(Object.prototype.hasOwnProperty.call(backing, '_registerIntegration')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(backing, 'version')).toBe(false);
    expect(Object.getOwnPropertyDescriptor(backing, 'que')).toMatchObject({
      configurable: true,
      enumerable: true,
      value: ingress,
      writable: false,
    });
  });

  it('rolls back a failed start claim without leaving a partial owner', () => {
    let fail = true;
    const backing = {};
    const target = new Proxy(backing, {
      defineProperty(object, key, descriptor) {
        if (fail) {
          fail = false;
          throw new Error('transient define failure');
        }
        return Reflect.defineProperty(object, key, descriptor);
      },
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });

    expect(runtime.start()).toBe(false);
    expect(runtime.state).toBe('unclaimed');
    expect(Object.prototype.hasOwnProperty.call(target, '_registerIntegration')).toBe(false);
    expect(
      createRuntime({
        target,
        releaseId: RELEASE,
        manifest: manifest([]),
        knownIntegrationIds: Object.freeze([]),
        boot: boot(),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      }).start()
    ).toBe(true);
  });

  it('captures the monotonic start before queue normalization work', async () => {
    let time = 0;
    const backing = {};
    const target = new Proxy(backing, {
      defineProperty(object, key, descriptor) {
        time = 10_000;
        return Reflect.defineProperty(object, key, descriptor);
      },
    });
    const runtime = createRuntime({
      target,
      releaseId: RELEASE,
      manifest: manifest([]),
      knownIntegrationIds: Object.freeze([]),
      boot: boot(),
      now: () => time,
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
    });
    expect(runtime.start()).toBe(true);

    await expect(runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
  });
});
