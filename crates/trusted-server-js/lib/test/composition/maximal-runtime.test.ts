import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  createNoopGoogletagAdapter,
  type GoogletagDiagnosticsObserver,
} from '../../src/adapters/googletag';
import { createNoopMessagingAdapter } from '../../src/adapters/messaging';
import { createNoopPrebidAdapter } from '../../src/adapters/prebid';
import { createTestBrowserRuntimeComposition } from '../../src/composition/browser_test';
import { createApsIntegrationRegistration } from '../../src/integrations/aps/module';
import { createCreativeIntegrationRegistration } from '../../src/integrations/creative/module';
import { createDataDomeIntegrationRegistration } from '../../src/integrations/datadome/module';
import { createDidomiIntegrationRegistration } from '../../src/integrations/didomi/module';
import { createGoogleTagManagerIntegrationRegistration } from '../../src/integrations/google_tag_manager/module';
import { createGptLaterIntegrationRegistration } from '../../src/integrations/gpt/later';
import { createGptIntegrationRegistration } from '../../src/integrations/gpt/module';
import { createGptDiagnosticsIntegrationRegistration } from '../../src/integrations/gpt_diagnostics/module';
import { createDiagnosticsPresentationIntegrationRegistration } from '../../src/integrations/gpt_diagnostics/presentation';
import { createLockrIntegrationRegistration } from '../../src/integrations/lockr/module';
import { createOsanoLifecycleIntegrationRegistration } from '../../src/integrations/osano/lifecycle';
import { createOsanoIntegrationRegistration } from '../../src/integrations/osano/module';
import { createPermutiveLifecycleIntegrationRegistration } from '../../src/integrations/permutive/lifecycle';
import { createPermutiveIntegrationRegistration } from '../../src/integrations/permutive/module';
import { createPrebidLaterIntegrationRegistration } from '../../src/integrations/prebid/later';
import { createPrebidIntegrationRegistration } from '../../src/integrations/prebid/module';
import { createRenderRuntimeIntegrationRegistration } from '../../src/integrations/render_runtime/module';
import { createSourcepointLifecycleIntegrationRegistration } from '../../src/integrations/sourcepoint/lifecycle';
import { createSourcepointIntegrationRegistration } from '../../src/integrations/sourcepoint/module';
import { createTestlightIntegrationRegistration } from '../../src/integrations/testlight/module';
import type { BootManifestV1 } from '../../src/core/types';
import type {
  IntegrationActivationContext,
  IntegrationCatalogEntry,
  IntegrationPrepareContext,
  IntegrationRegistration,
  PreparedIntegration,
} from '../../src/kernel/integration_registry';
import {
  MAX_CRITICAL_MODULES,
  MAX_MANIFEST_MODULES,
  RELEASE_CATALOG,
} from '../../src/kernel/release_catalog';

const TEST_RELEASE_ID = 'a'.repeat(64);
const EXPECTED_MAXIMAL_INTEGRATION_IDS = Object.freeze([
  'render_runtime',
  'aps',
  'creative',
  'datadome',
  'didomi',
  'google_tag_manager',
  'gpt',
  'gpt_diagnostics',
  'lockr',
  'osano_consent',
  'permutive_context',
  'sourcepoint_consent',
  'prebid',
  'testlight',
  'diagnostics_presentation',
  'gpt_later',
  'osano_lifecycle',
  'permutive_lifecycle',
  'prebid_later',
  'sourcepoint_lifecycle',
]);
const CRITICAL_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;
const DEFERRED_INTEGRATION_IDS = Object.freeze([
  'diagnostics_presentation',
  'gpt_later',
  'osano_lifecycle',
  'permutive_lifecycle',
  'prebid_later',
  'sourcepoint_lifecycle',
] as const);

type RegistrationFactory = (release: string) => IntegrationRegistration;

const REGISTRATION_FACTORIES = new Map<string, RegistrationFactory>([
  ['render_runtime', createRenderRuntimeIntegrationRegistration],
  ['aps', createApsIntegrationRegistration],
  ['creative', createCreativeIntegrationRegistration],
  ['datadome', createDataDomeIntegrationRegistration],
  ['didomi', createDidomiIntegrationRegistration],
  ['google_tag_manager', createGoogleTagManagerIntegrationRegistration],
  ['gpt', createGptIntegrationRegistration],
  ['gpt_diagnostics', createGptDiagnosticsIntegrationRegistration],
  ['lockr', createLockrIntegrationRegistration],
  ['osano_consent', createOsanoIntegrationRegistration],
  ['permutive_context', createPermutiveIntegrationRegistration],
  ['prebid', createPrebidIntegrationRegistration],
  ['sourcepoint_consent', createSourcepointIntegrationRegistration],
  ['testlight', createTestlightIntegrationRegistration],
  ['diagnostics_presentation', createDiagnosticsPresentationIntegrationRegistration],
  ['gpt_later', createGptLaterIntegrationRegistration],
  ['osano_lifecycle', createOsanoLifecycleIntegrationRegistration],
  ['permutive_lifecycle', createPermutiveLifecycleIntegrationRegistration],
  ['prebid_later', createPrebidLaterIntegrationRegistration],
  ['sourcepoint_lifecycle', createSourcepointLifecycleIntegrationRegistration],
]);

function maximalIntegrationIds(): readonly string[] {
  return Object.freeze(RELEASE_CATALOG.map(({ id }) => id));
}

function maximalManifest(): Readonly<BootManifestV1> {
  return Object.freeze({
    version: 1,
    releaseId: TEST_RELEASE_ID,
    criticalSrc: CRITICAL_SRC,
    integrations: Object.freeze(
      RELEASE_CATALOG.map(({ id, phase, trigger }) => {
        if (phase === 'critical') return Object.freeze({ id, phase });
        if (trigger !== 'first_display_or_idle') {
          throw new TypeError(`Deferred fixture ${id} is missing its canonical trigger`);
        }
        return Object.freeze({
          id,
          phase,
          trigger,
          src: `/static/tsjs=tsjs-${id}.min.js?v=${'d'.repeat(64)}`,
        });
      })
    ),
  });
}

function maximalRegistryCatalog(): readonly IntegrationCatalogEntry[] {
  return Object.freeze(
    RELEASE_CATALOG.map(({ id, phase, trigger, consumes, provides }) =>
      Object.freeze({ id, phase, trigger, consumes, provides })
    )
  );
}

function tracedRegistration(
  registration: IntegrationRegistration,
  events: string[],
  failAfterActivation?: string
): IntegrationRegistration {
  return Object.freeze({
    abi: registration.abi,
    id: registration.id,
    phase: registration.phase,
    releaseId: registration.releaseId,
    prepare: async (context: IntegrationPrepareContext) => {
      events.push(`prepare:${registration.id}`);
      const prepared = await registration.prepare(context);
      const traced: PreparedIntegration = {
        activate: (activationContext: IntegrationActivationContext): void => {
          events.push(`activate:${registration.id}`);
          activationContext.onDispose(() => events.push(`dispose:${registration.id}`));
          prepared.activate(activationContext);
          if (registration.id === failAfterActivation) {
            throw new Error(`injected ${registration.id} activation failure`);
          }
        },
      };
      const interfacesDescriptor = Object.getOwnPropertyDescriptor(prepared, 'interfaces');
      if (interfacesDescriptor) {
        Object.defineProperty(traced, 'interfaces', interfacesDescriptor);
      }
      return Object.freeze(traced);
    },
  });
}

function integrationConfig(id: string): unknown {
  if (id === 'didomi') return Object.freeze({ proxyPath: '/integrations/didomi/consent/' });
  if (id === 'gpt') return Object.freeze({});
  if (id === 'prebid') {
    return Object.freeze({
      clientSideBidders: Object.freeze([]),
      excludedGamAdUnitPathSuffixes: Object.freeze([]),
    });
  }
  if (id === 'sourcepoint_consent') return Object.freeze({ rewriteSdk: true });
  return undefined;
}

interface MaximalHarnessOptions {
  readonly blockDeferredId?: (typeof DEFERRED_INTEGRATION_IDS)[number];
  readonly configOverrides?: Readonly<Record<string, unknown>>;
  readonly failAfterActivation?: string;
}

interface TrackedListener {
  readonly capture: boolean;
  readonly listener: EventListenerOrEventListenerObject;
  readonly target: EventTarget;
  readonly type: string;
}

interface TrackedDeferredDeadline {
  active: boolean;
  readonly handle: ReturnType<typeof setTimeout>;
  id?: string;
  readonly identity: number;
  readonly startedAt: number;
}

function captureOption(options?: boolean | AddEventListenerOptions): boolean {
  return typeof options === 'boolean' ? options : options?.capture === true;
}

function createMaximalHarness(options: MaximalHarnessOptions = {}) {
  const integrationIds = maximalIntegrationIds();
  const events: string[] = [];
  const registrations = integrationIds.map((id) => {
    const factory = REGISTRATION_FACTORIES.get(id);
    if (!factory) throw new Error(`Missing real registration factory for ${id}`);
    return tracedRegistration(factory(TEST_RELEASE_ID), events, options.failAfterActivation);
  });
  const registrationsById = new Map(
    registrations.map((registration) => [registration.id, registration])
  );
  const criticalRegistrations = registrations.filter(({ phase }) => phase === 'critical');
  const deferredRegistrations = registrations.filter(({ phase }) => phase === 'deferred');
  const frames: FrameRequestCallback[] = [];
  const idle: Array<() => void> = [];
  const deferredDeadlines: TrackedDeferredDeadline[] = [];
  let nextDeferredDeadlineIdentity = 1;
  const trackedSetTimeout = (
    callback: () => void,
    delayMs: number
  ): ReturnType<typeof setTimeout> => {
    let deadline: TrackedDeferredDeadline | undefined;
    const handle = setTimeout(() => {
      if (deadline) deadline.active = false;
      callback();
    }, delayMs);
    if (delayMs === 10_000) {
      deadline = {
        active: true,
        handle,
        identity: nextDeferredDeadlineIdentity,
        startedAt: Date.now(),
      };
      nextDeferredDeadlineIdentity += 1;
      deferredDeadlines.push(deadline);
    }
    return handle;
  };
  const trackedClearTimeout = (handle: ReturnType<typeof setTimeout>): void => {
    const deadline = deferredDeadlines.find((candidate) => candidate.handle === handle);
    if (deadline) deadline.active = false;
    clearTimeout(handle);
  };
  // JSDOM lazily installs its selector engine's own document-scoped listeners.
  // Materialize that test-environment infrastructure before tracking runtime effects.
  document.querySelectorAll('[id]');
  const activeObservers = new Set<GoogletagDiagnosticsObserver>();
  const activeMutationObservers = new Set<MutationObserver>();
  const listenerRecords: TrackedListener[] = [];
  const eventTargetPrototype = EventTarget.prototype;
  const addDescriptor = Object.getOwnPropertyDescriptor(eventTargetPrototype, 'addEventListener');
  const removeDescriptor = Object.getOwnPropertyDescriptor(
    eventTargetPrototype,
    'removeEventListener'
  );
  if (
    !addDescriptor ||
    !('value' in addDescriptor) ||
    typeof addDescriptor.value !== 'function' ||
    !removeDescriptor ||
    !('value' in removeDescriptor) ||
    typeof removeDescriptor.value !== 'function'
  ) {
    throw new Error('EventTarget listener intrinsics are unavailable');
  }
  const nativeAdd = addDescriptor.value as EventTarget['addEventListener'];
  const nativeRemove = removeDescriptor.value as EventTarget['removeEventListener'];
  Object.defineProperty(eventTargetPrototype, 'addEventListener', {
    ...addDescriptor,
    value: function (
      this: EventTarget,
      type: string,
      listener: EventListenerOrEventListenerObject,
      listenerOptions?: boolean | AddEventListenerOptions
    ): void {
      Reflect.apply(nativeAdd, this, [type, listener, listenerOptions]);
      if (this !== window && this !== document) return;
      const capture = captureOption(listenerOptions);
      if (
        !listenerRecords.some(
          (record) =>
            record.target === this &&
            record.type === type &&
            record.listener === listener &&
            record.capture === capture
        )
      ) {
        listenerRecords.push({ capture, listener, target: this, type });
      }
    },
  });
  Object.defineProperty(eventTargetPrototype, 'removeEventListener', {
    ...removeDescriptor,
    value: function (
      this: EventTarget,
      type: string,
      listener: EventListenerOrEventListenerObject,
      listenerOptions?: boolean | EventListenerOptions
    ): void {
      Reflect.apply(nativeRemove, this, [type, listener, listenerOptions]);
      const capture = captureOption(listenerOptions);
      const index = listenerRecords.findIndex(
        (record) =>
          record.target === this &&
          record.type === type &&
          record.listener === listener &&
          record.capture === capture
      );
      if (index >= 0) listenerRecords.splice(index, 1);
    },
  });

  const NativeMutationObserver = window.MutationObserver;
  class TrackedMutationObserver extends NativeMutationObserver {
    public constructor(callback: MutationCallback) {
      super(callback);
      activeMutationObservers.add(this);
    }

    public override disconnect(): void {
      activeMutationObservers.delete(this);
      super.disconnect();
    }
  }
  vi.stubGlobal('MutationObserver', TrackedMutationObserver);

  let activeCaptureListeners = 0;
  const captureListenerIdentities = new Set<(event: MessageEvent) => void>();
  const googletag = Object.freeze({
    ...createNoopGoogletagAdapter(),
    observeDiagnostics: (observer: GoogletagDiagnosticsObserver) => {
      activeObservers.add(observer);
      return (): void => {
        activeObservers.delete(observer);
      };
    },
  });
  const messaging = Object.freeze({
    ...createNoopMessagingAdapter(),
    installCaptureListener: (listener: (event: MessageEvent) => void) => {
      activeCaptureListeners += 1;
      captureListenerIdentities.add(listener);
      window.addEventListener('message', listener, true);
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        activeCaptureListeners -= 1;
        captureListenerIdentities.delete(listener);
        window.removeEventListener('message', listener, true);
      };
    },
  });
  const target: Record<string, unknown> = {};
  const appendChildBefore = Element.prototype.appendChild;
  const insertBeforeBefore = Element.prototype.insertBefore;
  const fetchBefore = Object.getOwnPropertyDescriptor(window, 'fetch');
  const sendBeaconBefore = Object.getOwnPropertyDescriptor(navigator, 'sendBeacon');
  const didomiBefore = Object.getOwnPropertyDescriptor(window, 'didomiConfig');
  const testlightBefore = Object.getOwnPropertyDescriptor(window, 'testlight');
  const composition = createTestBrowserRuntimeComposition(
    {
      target,
      releaseId: TEST_RELEASE_ID,
      manifest: maximalManifest(),
      knownIntegrationIds: integrationIds,
      catalog: maximalRegistryCatalog(),
      boot: {
        auctionProjection: {
          version: 1,
          auction: { version: 1, auctionId: 'initial', results: [] },
          slots: [],
          bids: [],
        },
        creative: { version: 1, enabled: true, clickGuard: true, renderGuard: false },
        diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: true } },
      },
      getBindings: (id) =>
        Object.freeze({
          config:
            options.configOverrides !== undefined &&
            Object.prototype.hasOwnProperty.call(options.configOverrides, id)
              ? options.configOverrides?.[id]
              : integrationConfig(id),
          interfaces: Object.freeze({}),
        }),
      kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      phaseScheduler: {
        cancelAnimationFrame: vi.fn(),
        cancelIdleCallback: vi.fn(),
        clearTimeout: trackedClearTimeout,
        requestAnimationFrame: (callback) => {
          frames.push(callback);
          return frames.length;
        },
        requestIdleCallback: (callback) => {
          idle.push(callback);
          return idle.length;
        },
        setTimeout: trackedSetTimeout,
      },
    },
    {
      adapters: {
        googletag,
        messaging,
        prebid: createNoopPrebidAdapter(),
      },
      coreActivations: { correctnessGptListeners: vi.fn() },
    }
  );

  expect(composition.runtime.start()).toBe(true);
  expect(composition.runtime.start()).toBe(false);
  for (const registration of criticalRegistrations) {
    expect(
      composition.runtime.registerIntegration(registration),
      `register ${registration.id}`
    ).toBe(true);
    events.push(`register:${registration.id}`);
  }
  const criticalScript = document.querySelector<HTMLScriptElement>('#trustedserver-js');
  if (!criticalScript) throw new Error('Maximal fixture critical script is unavailable');
  const nativeHeadAppend = document.head.append.bind(document.head);
  const appendDeferred = vi.spyOn(document.head, 'append').mockImplementation((...nodes) => {
    nativeHeadAppend(...nodes);
    for (const node of nodes) {
      if (!(node instanceof HTMLScriptElement) || node === criticalScript) continue;
      const matchedId = /\/static\/tsjs=tsjs-([a-z0-9_-]+)\.min\.js$/.exec(
        new URL(node.src).pathname
      )?.[1];
      const registration = matchedId ? registrationsById.get(matchedId) : undefined;
      if (!registration || registration.phase !== 'deferred') {
        throw new Error(`Unexpected deferred artifact ${node.src}`);
      }
      const deadline = [...deferredDeadlines]
        .reverse()
        .find((candidate) => candidate.active && candidate.id === undefined);
      if (!deadline) throw new Error(`Deferred deadline is unavailable for ${registration.id}`);
      deadline.id = registration.id;
      if (registration.id === options.blockDeferredId) continue;
      Object.defineProperty(document, 'currentScript', { configurable: true, value: node });
      expect(
        composition.runtime.registerIntegration(registration),
        `register deferred ${registration.id}`
      ).toBe(true);
      events.push(`register:${registration.id}`);
      node.onload?.(new Event('load'));
      Object.defineProperty(document, 'currentScript', {
        configurable: true,
        value: criticalScript,
      });
    }
  });

  const loadDeferred = async (): Promise<void> => {
    expect(composition.runtime.protectFirstDisplayAttemptBatch([Promise.resolve()])).toBe(true);
    await Promise.resolve();
    await Promise.resolve();
    expect(frames).toHaveLength(1);
    frames.shift()?.(1);
    expect(frames).toHaveLength(1);
    frames.shift()?.(2);
    expect(idle).toHaveLength(1);
    idle.shift()?.();
    const expectedDeferredIds = deferredRegistrations
      .map(({ id }) => id)
      .filter((id) => id !== options.blockDeferredId);
    await vi.waitFor(() => {
      expect(
        events
          .filter((event) => event.startsWith('register:'))
          .filter((event) => deferredRegistrations.some(({ id }) => event === `register:${id}`))
      ).toEqual(expectedDeferredIds.map((id) => `register:${id}`));
      expect(deferredDeadlines.filter(({ id }) => id !== undefined)).toHaveLength(
        deferredRegistrations.length
      );
    });
  };

  const assertReleased = async (): Promise<void> => {
    composition.runtime.dispose();
    composition.runtime.dispose();
    await Promise.resolve();
    expect(activeObservers.size).toBe(0);
    expect(activeMutationObservers.size).toBe(0);
    expect(activeCaptureListeners).toBe(0);
    expect(
      listenerRecords.map(({ capture, target: listenerTarget, type }) => ({
        capture,
        target: listenerTarget.constructor.name,
        type,
      }))
    ).toEqual([]);
    expect(composition.auctionContextRegistryForTest()).toBeUndefined();
    expect(vi.getTimerCount()).toBe(0);
    expect(Element.prototype.appendChild).toBe(appendChildBefore);
    expect(Element.prototype.insertBefore).toBe(insertBeforeBefore);
    expect(Object.getOwnPropertyDescriptor(window, 'fetch')).toEqual(fetchBefore);
    expect(Object.getOwnPropertyDescriptor(navigator, 'sendBeacon')).toEqual(sendBeaconBefore);
    expect(Object.getOwnPropertyDescriptor(window, 'didomiConfig')).toEqual(didomiBefore);
    expect(Object.getOwnPropertyDescriptor(window, 'testlight')).toEqual(testlightBefore);
  };

  const restoreInstrumentation = (): void => {
    for (const record of [...listenerRecords]) {
      try {
        Reflect.apply(nativeRemove, record.target, [record.type, record.listener, record.capture]);
      } catch {
        // Test cleanup must not hide the first assertion failure.
      }
    }
    listenerRecords.length = 0;
    for (const observer of [...activeMutationObservers]) observer.disconnect();
    appendDeferred.mockRestore();
    Object.defineProperty(eventTargetPrototype, 'addEventListener', addDescriptor);
    Object.defineProperty(eventTargetPrototype, 'removeEventListener', removeDescriptor);
  };

  return Object.freeze({
    assertReleased,
    criticalIntegrationIds: criticalRegistrations.map(({ id }) => id),
    composition,
    deferredDeadlines: () =>
      Object.freeze(
        deferredDeadlines.flatMap(({ active, id, identity, startedAt }) =>
          id === undefined ? [] : [Object.freeze({ active, id, identity, startedAt })]
        )
      ),
    deferredIntegrationIds: deferredRegistrations.map(({ id }) => id),
    events,
    integrationIds,
    loadDeferred,
    ownershipIdentities: () =>
      Object.freeze({
        adapters: composition.adapters,
        captureListeners: Object.freeze([...captureListenerIdentities]),
        runtime: composition.runtime,
      }),
    resourceCounts: () =>
      Object.freeze({
        captureListeners: activeCaptureListeners,
        listeners: listenerRecords.length,
        mutationObservers: activeMutationObservers.size,
        observers: activeObservers.size,
      }),
    restoreInstrumentation,
    target,
  });
}

describe('generated maximal browser runtime transaction', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('derives the complete maximal fixture in canonical release order', () => {
    const integrationIds = maximalIntegrationIds();
    const manifest = maximalManifest();

    expect(integrationIds).toEqual(EXPECTED_MAXIMAL_INTEGRATION_IDS);
    expect(integrationIds).toHaveLength(MAX_MANIFEST_MODULES);
    expect(manifest).toMatchObject({
      version: 1,
      releaseId: TEST_RELEASE_ID,
      criticalSrc: CRITICAL_SRC,
    });
    expect(manifest.integrations.map(({ id }) => id)).toEqual(integrationIds);
    expect(
      manifest.integrations
        .slice(0, MAX_CRITICAL_MODULES)
        .every(({ phase }) => phase === 'critical')
    ).toBe(true);
    expect(
      manifest.integrations.slice(MAX_CRITICAL_MODULES).every(({ phase }) => phase === 'deferred')
    ).toBe(true);
    for (const entry of manifest.integrations) {
      expect(Object.isFrozen(entry)).toBe(true);
      if (entry.phase === 'critical') {
        expect(Reflect.ownKeys(entry).sort()).toEqual(['id', 'phase']);
      } else {
        expect(Reflect.ownKeys(entry).sort()).toEqual(['id', 'phase', 'src', 'trigger']);
        expect(entry.trigger).toBe('first_display_or_idle');
        expect(entry.src).toBe(`/static/tsjs=tsjs-${entry.id}.min.js?v=${'d'.repeat(64)}`);
      }
    }
    expect(Object.isFrozen(manifest)).toBe(true);
    expect(Object.isFrozen(manifest.integrations)).toBe(true);
  });

  it.each([
    ['provider', true],
    ['non-provider', false],
  ] as const)(
    'preserves exact prepared interfaces for a %s registration',
    async (_name, provider) => {
      const capability = Object.freeze({ invoke: vi.fn() });
      const providerInterfaces = Object.freeze({ 'fixture.v1': capability });
      const activate = vi.fn();
      const registration: IntegrationRegistration = Object.freeze({
        abi: 1,
        id: 'fixture',
        phase: 'critical',
        releaseId: TEST_RELEASE_ID,
        prepare: async () =>
          provider
            ? Object.freeze({ activate, interfaces: providerInterfaces })
            : Object.freeze({ activate }),
      });
      const prepared = await tracedRegistration(registration, []).prepare({
        config: undefined,
        interfaces: Object.freeze({}),
        signal: new AbortController().signal,
        onDispose: vi.fn(),
      });

      expect(Object.isFrozen(prepared)).toBe(true);
      expect(Reflect.ownKeys(prepared).sort()).toEqual(
        provider ? ['activate', 'interfaces'] : ['activate']
      );
      if (provider) {
        expect(Object.getOwnPropertyDescriptor(prepared, 'interfaces')).toMatchObject({
          enumerable: true,
          value: providerInterfaces,
        });
        expect(prepared.interfaces).toBe(providerInterfaces);
        expect(prepared.interfaces?.['fixture.v1']).toBe(capability);
      } else {
        expect(Object.prototype.hasOwnProperty.call(prepared, 'interfaces')).toBe(false);
      }
    }
  );

  it('owns all server bundles once and disposes them in exact reverse generated order', async () => {
    vi.useFakeTimers();
    const harness = createMaximalHarness();
    try {
      const installed = await harness.composition.runtime.install();
      if (installed.state === 'fallback') {
        throw new Error(`${installed.reason}: ${harness.events.join(',')}`);
      }
      expect(installed).toEqual({
        state: 'kernel',
        runtimeFailures: [],
        dispose: expect.any(Function),
      });
      expect(harness.composition.runtime.state).toBe('kernel');

      await harness.loadDeferred();
      expect(harness.events.filter((event) => event.startsWith('register:'))).toEqual(
        harness.integrationIds.map((id) => `register:${id}`)
      );
      expect(harness.events.filter((event) => event.startsWith('prepare:'))).toEqual(
        harness.integrationIds.map((id) => `prepare:${id}`)
      );
      expect(harness.events.filter((event) => event.startsWith('activate:'))).toEqual(
        harness.integrationIds.map((id) => `activate:${id}`)
      );

      window.dispatchEvent(new Event('resize'));
      harness.composition.runtime.dispose();
      harness.composition.runtime.dispose();
      await Promise.resolve();
      expect(harness.events.filter((event) => event.startsWith('dispose:'))).toEqual(
        [...harness.integrationIds].reverse().map((id) => `dispose:${id}`)
      );
      await harness.assertReleased();
    } finally {
      harness.restoreInstrumentation();
    }
  });

  it.each(DEFERRED_INTEGRATION_IDS)(
    'starts five deferred siblings independently while %s remains blocked to its own deadline',
    async (blockedId) => {
      vi.useFakeTimers();
      const harness = createMaximalHarness({ blockDeferredId: blockedId });
      try {
        const installed = await harness.composition.runtime.install();
        expect(installed.state).toBe('kernel');
        const ownershipBefore = harness.ownershipIdentities();
        expect(ownershipBefore.captureListeners).toHaveLength(1);

        await harness.loadDeferred();

        const activeIds = harness.integrationIds.filter((id) => id !== blockedId);
        await vi.waitFor(() => {
          expect(harness.events.filter((event) => event.startsWith('register:'))).toEqual(
            activeIds.map((id) => `register:${id}`)
          );
          expect(harness.events.filter((event) => event.startsWith('prepare:'))).toEqual(
            activeIds.map((id) => `prepare:${id}`)
          );
          expect(harness.events.filter((event) => event.startsWith('activate:'))).toEqual(
            activeIds.map((id) => `activate:${id}`)
          );
        });

        const startedDeadlines = harness.deferredDeadlines();
        expect(startedDeadlines.map(({ id }) => id)).toEqual(DEFERRED_INTEGRATION_IDS);
        expect(new Set(startedDeadlines.map(({ identity }) => identity).values()).size).toBe(6);
        expect(new Set(startedDeadlines.map(({ startedAt }) => startedAt).values()).size).toBe(1);
        expect(startedDeadlines.filter(({ active }) => active).map(({ id }) => id)).toEqual([
          blockedId,
        ]);
        expect(document.querySelector(`script[src*="tsjs-${blockedId}.min.js"]`)).not.toBeNull();
        expect(harness.ownershipIdentities()).toEqual(ownershipBefore);

        const elapsedSinceStart = Date.now() - (startedDeadlines[0]?.startedAt ?? Date.now());
        await vi.advanceTimersByTimeAsync(9_999 - elapsedSinceStart);
        expect(harness.deferredDeadlines().find(({ id }) => id === blockedId)?.active).toBe(true);
        await vi.advanceTimersByTimeAsync(1);
        expect(harness.deferredDeadlines().find(({ id }) => id === blockedId)?.active).toBe(false);
        expect(document.querySelector(`script[src*="tsjs-${blockedId}.min.js"]`)).toBeNull();
        expect(harness.events.filter((event) => event.startsWith('dispose:'))).toEqual([]);
        expect(harness.ownershipIdentities()).toEqual(ownershipBefore);

        await harness.assertReleased();
        expect(harness.events.filter((event) => event.startsWith('dispose:'))).toEqual(
          [...activeIds].reverse().map((id) => `dispose:${id}`)
        );
      } finally {
        harness.composition.runtime.dispose();
        await Promise.resolve();
        harness.restoreInstrumentation();
      }
    }
  );

  it.each(DEFERRED_INTEGRATION_IDS)(
    'contains an acquired %s failure without delaying or replacing deferred siblings',
    async (failureId) => {
      vi.useFakeTimers();
      const harness = createMaximalHarness({ failAfterActivation: failureId });
      try {
        const installed = await harness.composition.runtime.install();
        expect(installed.state).toBe('kernel');
        const ownershipBefore = harness.ownershipIdentities();
        expect(ownershipBefore.captureListeners).toHaveLength(1);

        await harness.loadDeferred();

        await vi.waitFor(() => {
          expect(harness.events.filter((event) => event.startsWith('register:'))).toEqual(
            harness.integrationIds.map((id) => `register:${id}`)
          );
          expect(harness.events.filter((event) => event.startsWith('prepare:'))).toEqual(
            harness.integrationIds.map((id) => `prepare:${id}`)
          );
          expect(harness.events.filter((event) => event.startsWith('activate:'))).toEqual(
            harness.integrationIds.map((id) => `activate:${id}`)
          );
          expect(harness.events.filter((event) => event.startsWith('dispose:'))).toEqual([
            `dispose:${failureId}`,
          ]);
        });

        const startedDeadlines = harness.deferredDeadlines();
        expect(startedDeadlines.map(({ id }) => id)).toEqual(DEFERRED_INTEGRATION_IDS);
        expect(new Set(startedDeadlines.map(({ identity }) => identity).values()).size).toBe(6);
        expect(new Set(startedDeadlines.map(({ startedAt }) => startedAt).values()).size).toBe(1);
        expect(startedDeadlines.some(({ active }) => active)).toBe(false);
        expect(harness.ownershipIdentities()).toEqual(ownershipBefore);

        await harness.assertReleased();
        const disposalEvents = harness.events.filter((event) => event.startsWith('dispose:'));
        expect(disposalEvents).toHaveLength(harness.integrationIds.length);
        for (const id of harness.integrationIds) {
          expect(disposalEvents.filter((event) => event === `dispose:${id}`)).toHaveLength(1);
        }
      } finally {
        harness.composition.runtime.dispose();
        await Promise.resolve();
        harness.restoreInstrumentation();
      }
    }
  );

  it.each([
    {
      name: 'a real activation fails after acquiring its composed effects',
      failureId: 'permutive_context',
      phase: 'activate' as const,
    },
    {
      name: 'one real registration receives malformed frozen config',
      failureId: 'sourcepoint_consent',
      phase: 'prepare' as const,
    },
  ])('fails closed when $name', async ({ failureId, phase }) => {
    vi.useFakeTimers();
    const harness = createMaximalHarness(
      phase === 'activate'
        ? { failAfterActivation: failureId }
        : {
            configOverrides: Object.freeze({
              [failureId]: Object.freeze({ rewriteSdk: 'yes' }),
            }),
          }
    );
    try {
      const installed = await harness.composition.runtime.install();
      const failureIndex = harness.criticalIntegrationIds.indexOf(failureId);
      const preparedIds =
        phase === 'activate'
          ? harness.criticalIntegrationIds
          : harness.criticalIntegrationIds.slice(0, failureIndex + 1);
      const activatedIds =
        phase === 'activate' ? harness.criticalIntegrationIds.slice(0, failureIndex + 1) : [];

      expect(installed).toEqual({ state: 'fallback', reason: 'bundle_partial' });
      expect(harness.composition.runtime.state).toBe('fallback');
      expect(harness.target['_internal']).toMatchObject({
        state: 'fallback',
        reason: 'bundle_partial',
      });
      expect(harness.events.filter((event) => event.startsWith('register:'))).toEqual(
        harness.criticalIntegrationIds.map((id) => `register:${id}`)
      );
      expect(harness.events.filter((event) => event.startsWith('prepare:'))).toEqual(
        preparedIds.map((id) => `prepare:${id}`)
      );
      expect(harness.events.filter((event) => event.startsWith('activate:'))).toEqual(
        activatedIds.map((id) => `activate:${id}`)
      );
      expect(harness.events.filter((event) => event.startsWith('dispose:'))).toEqual(
        [...activatedIds].reverse().map((id) => `dispose:${id}`)
      );

      await harness.assertReleased();
      expect(harness.events.filter((event) => event.startsWith('dispose:'))).toEqual(
        [...activatedIds].reverse().map((id) => `dispose:${id}`)
      );
    } finally {
      harness.restoreInstrumentation();
    }
  });

  it.each([
    { name: 'missing SDK globals reach their bounded readiness timeouts', kind: 'readiness' },
    { name: 'hostile consent storage fails only its after-commit owner', kind: 'storage' },
    {
      name: 'matcher false positives and throwing publisher callbacks stay isolated',
      kind: 'matcher',
    },
  ] as const)('isolates $name across all real registrations', async ({ kind }) => {
    vi.useFakeTimers();
    const callbackOrder: string[] = [];
    const publisherBinding: { target?: Record<string, unknown> } = {};
    let falsePositiveScript: HTMLScriptElement | undefined;
    if (kind === 'readiness') {
      vi.stubGlobal('identityLockr', undefined);
      vi.stubGlobal('permutive', undefined);
    }
    if (kind === 'storage') {
      vi.stubGlobal(
        'localStorage',
        new Proxy({} as Storage, {
          get: () => {
            throw new Error('publisher storage is unavailable');
          },
        })
      );
    }
    if (kind === 'matcher') {
      vi.stubGlobal('testlight', {
        que: [
          function (this: unknown): void {
            callbackOrder.push(this === publisherBinding.target ? 'throw:bound' : 'throw:unbound');
            throw new Error('publisher queue callback failed');
          },
          function (this: unknown): void {
            callbackOrder.push(
              this === publisherBinding.target ? 'survive:bound' : 'survive:unbound'
            );
          },
        ],
      });
    }
    const harness = createMaximalHarness();
    publisherBinding.target = harness.target;
    try {
      const installed = await harness.composition.runtime.install();
      const expectedRuntimeFailures =
        kind === 'storage' ? [{ id: 'sourcepoint_consent', phase: 'after_commit' }] : [];
      const activeIntegrationIds =
        kind === 'storage'
          ? harness.integrationIds.filter((id) => id !== 'sourcepoint_lifecycle')
          : harness.integrationIds;

      expect(installed).toEqual({
        state: 'kernel',
        runtimeFailures: expectedRuntimeFailures,
        dispose: expect.any(Function),
      });
      expect(harness.composition.runtime.state).toBe('kernel');
      await harness.loadDeferred();
      expect(harness.events.filter((event) => event.startsWith('prepare:'))).toEqual(
        activeIntegrationIds.map((id) => `prepare:${id}`)
      );
      expect(harness.events.filter((event) => event.startsWith('activate:'))).toEqual(
        activeIntegrationIds.map((id) => `activate:${id}`)
      );
      expect(harness.resourceCounts()).toMatchObject({
        captureListeners: 1,
        listeners: expect.any(Number),
        mutationObservers: expect.any(Number),
        observers: 1,
      });
      expect(harness.resourceCounts().listeners).toBeGreaterThan(0);
      expect(harness.resourceCounts().mutationObservers).toBeGreaterThan(0);

      if (kind === 'readiness') {
        await vi.runAllTimersAsync();
        expect(harness.composition.runtime.state).toBe('kernel');
        expect(vi.getTimerCount()).toBe(0);
      }
      if (kind === 'matcher') {
        expect(callbackOrder).toEqual(['throw:bound', 'survive:bound']);
        falsePositiveScript = document.createElement('script');
        const originalUrl = 'https://publisher.example/assets/www.googletagmanager.com/gtm.js';
        falsePositiveScript.src = originalUrl;
        document.head.appendChild(falsePositiveScript);
        expect(falsePositiveScript.src).toBe(originalUrl);
      }

      falsePositiveScript?.remove();
      const disposedBeforeRuntimeRelease =
        kind === 'storage' ? ['dispose:sourcepoint_consent'] : [];
      expect(harness.events.filter((event) => event.startsWith('dispose:'))).toEqual(
        disposedBeforeRuntimeRelease
      );
      await harness.assertReleased();
      const reverseIds = [...activeIntegrationIds].reverse();
      const expectedDisposals =
        kind === 'storage'
          ? [
              'dispose:sourcepoint_consent',
              ...reverseIds
                .filter((id) => id !== 'sourcepoint_consent')
                .map((id) => `dispose:${id}`),
            ]
          : reverseIds.map((id) => `dispose:${id}`);
      expect(harness.events.filter((event) => event.startsWith('dispose:'))).toEqual(
        expectedDisposals
      );
    } finally {
      falsePositiveScript?.remove();
      harness.restoreInstrumentation();
    }
  });
});
