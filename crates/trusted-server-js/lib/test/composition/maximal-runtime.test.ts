import path from 'node:path';

import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  createNoopGoogletagAdapter,
  type GoogletagDiagnosticsObserver,
} from '../../src/adapters/googletag';
import { createNoopMessagingAdapter } from '../../src/adapters/messaging';
import { createNoopPrebidAdapter } from '../../src/adapters/prebid';
import { createTestBrowserRuntimeComposition } from '../../src/composition/browser';
import { createCreativeIntegrationRegistration } from '../../src/integrations/creative/module';
import { createDataDomeIntegrationRegistration } from '../../src/integrations/datadome/module';
import { createDidomiIntegrationRegistration } from '../../src/integrations/didomi/module';
import { createGoogleTagManagerIntegrationRegistration } from '../../src/integrations/google_tag_manager/module';
import { createGptIntegrationRegistration } from '../../src/integrations/gpt/module';
import { createGptDiagnosticsIntegrationRegistration } from '../../src/integrations/gpt_diagnostics/module';
import { createLockrIntegrationRegistration } from '../../src/integrations/lockr/module';
import { createOsanoIntegrationRegistration } from '../../src/integrations/osano/module';
import { createPermutiveIntegrationRegistration } from '../../src/integrations/permutive/module';
import { createPrebidIntegrationRegistration } from '../../src/integrations/prebid/module';
import { createSourcepointIntegrationRegistration } from '../../src/integrations/sourcepoint/module';
import { createTestlightIntegrationRegistration } from '../../src/integrations/testlight/module';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../src/kernel/integration_registry';
import { discoverIntegrationModules } from '../../scripts/integration-inventory-v1.mjs';

const TEST_RELEASE_ID = 'a'.repeat(64);

type RegistrationFactory = (release: string) => IntegrationRegistration;

const REGISTRATION_FACTORIES = new Map<string, RegistrationFactory>([
  ['creative', createCreativeIntegrationRegistration],
  ['datadome', createDataDomeIntegrationRegistration],
  ['didomi', createDidomiIntegrationRegistration],
  ['google_tag_manager', createGoogleTagManagerIntegrationRegistration],
  ['gpt', createGptIntegrationRegistration],
  ['gpt_diagnostics', createGptDiagnosticsIntegrationRegistration],
  ['lockr', createLockrIntegrationRegistration],
  ['osano', createOsanoIntegrationRegistration],
  ['permutive', createPermutiveIntegrationRegistration],
  ['prebid', createPrebidIntegrationRegistration],
  ['sourcepoint', createSourcepointIntegrationRegistration],
  ['testlight', createTestlightIntegrationRegistration],
]);

function generatedIntegrationIds(): readonly string[] {
  return Object.freeze(discoverIntegrationModules(path.resolve(process.cwd(), 'src/integrations')));
}

function tracedRegistration(
  registration: IntegrationRegistration,
  events: string[],
  failAfterActivation?: string
): IntegrationRegistration {
  return Object.freeze({
    id: registration.id,
    release: registration.release,
    prepare: async (context: IntegrationPrepareContext) => {
      events.push(`prepare:${registration.id}`);
      const prepared = await registration.prepare(context);
      return Object.freeze({
        activate: (activationContext: IntegrationActivationContext): void => {
          events.push(`activate:${registration.id}`);
          activationContext.onDispose(() => events.push(`dispose:${registration.id}`));
          prepared.activate(activationContext);
          if (registration.id === failAfterActivation) {
            throw new Error(`injected ${registration.id} activation failure`);
          }
        },
      });
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
  if (id === 'sourcepoint') return Object.freeze({ rewriteSdk: true });
  return undefined;
}

interface MaximalHarnessOptions {
  readonly configOverrides?: Readonly<Record<string, unknown>>;
  readonly failAfterActivation?: string;
}

interface TrackedListener {
  readonly capture: boolean;
  readonly listener: EventListenerOrEventListenerObject;
  readonly target: EventTarget;
  readonly type: string;
}

function captureOption(options?: boolean | AddEventListenerOptions): boolean {
  return typeof options === 'boolean' ? options : options?.capture === true;
}

function createMaximalHarness(options: MaximalHarnessOptions = {}) {
  const integrationIds = generatedIntegrationIds();
  const events: string[] = [];
  const registrations = integrationIds.map((id) => {
    const factory = REGISTRATION_FACTORIES.get(id);
    if (!factory) throw new Error(`Missing real registration factory for ${id}`);
    return tracedRegistration(factory(TEST_RELEASE_ID), events, options.failAfterActivation);
  });
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
      window.addEventListener('message', listener, true);
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        activeCaptureListeners -= 1;
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
      manifest: {
        version: 1,
        releaseId: TEST_RELEASE_ID,
        integrations: integrationIds.map((id) => ({ id, required: true })),
      },
      knownIntegrationIds: integrationIds,
      boot: {
        auctionProjection: {
          version: 1,
          auction: { version: 1, auctionId: 'initial', results: [] },
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
  for (const registration of registrations) {
    expect(composition.runtime.registerIntegration(registration)).toBe(true);
    events.push(`register:${registration.id}`);
  }

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
    Object.defineProperty(eventTargetPrototype, 'addEventListener', addDescriptor);
    Object.defineProperty(eventTargetPrototype, 'removeEventListener', removeDescriptor);
  };

  return Object.freeze({
    assertReleased,
    composition,
    events,
    integrationIds,
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

  it('owns all server bundles once and disposes them in exact reverse generated order', async () => {
    vi.useFakeTimers();
    const integrationIds = generatedIntegrationIds();
    const events: string[] = [];
    const registrations = integrationIds.map((id) => {
      const factory = REGISTRATION_FACTORIES.get(id);
      if (!factory) throw new Error(`Missing real registration factory for ${id}`);
      return tracedRegistration(factory(TEST_RELEASE_ID), events);
    });
    const activeObservers = new Set<GoogletagDiagnosticsObserver>();
    const activeMutationObservers = new Set<MutationObserver>();
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
    const googletag = Object.freeze({
      ...createNoopGoogletagAdapter(),
      observeDiagnostics: (observer: GoogletagDiagnosticsObserver) => {
        activeObservers.add(observer);
        return (): void => {
          activeObservers.delete(observer);
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
        manifest: {
          version: 1,
          releaseId: TEST_RELEASE_ID,
          integrations: integrationIds.map((id) => ({ id, required: true })),
        },
        knownIntegrationIds: integrationIds,
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            bids: [],
          },
          creative: { version: 1, enabled: true, clickGuard: true, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: true } },
        },
        getBindings: (id) =>
          Object.freeze({ config: integrationConfig(id), interfaces: Object.freeze({}) }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag,
          messaging: Object.freeze({
            ...createNoopMessagingAdapter(),
            installCaptureListener: () => {
              activeCaptureListeners += 1;
              let active = true;
              return (): void => {
                if (!active) return;
                active = false;
                activeCaptureListeners -= 1;
              };
            },
          }),
          prebid: createNoopPrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    expect(composition.runtime.start()).toBe(false);
    for (const registration of registrations) {
      expect(registration.release).toBe(TEST_RELEASE_ID);
      expect(composition.runtime.registerIntegration(registration)).toBe(true);
      events.push(`register:${registration.id}`);
    }

    const installed = await composition.runtime.install();

    if (installed.state === 'fallback') {
      throw new Error(`${installed.reason}: ${events.join(',')}`);
    }
    expect(installed).toEqual({
      state: 'kernel',
      runtimeFailures: [],
      dispose: expect.any(Function),
    });
    expect(composition.runtime.state).toBe('kernel');
    expect(target['releaseId']).toBe(TEST_RELEASE_ID);
    expect(events.filter((event) => event.startsWith('register:'))).toEqual(
      integrationIds.map((id) => `register:${id}`)
    );
    expect(events.filter((event) => event.startsWith('prepare:'))).toEqual(
      integrationIds.map((id) => `prepare:${id}`)
    );
    expect(events.filter((event) => event.startsWith('activate:'))).toEqual(
      integrationIds.map((id) => `activate:${id}`)
    );
    expect(composition.runtimeSessionForTest()?.interfaces).toMatchObject(
      Object.fromEntries(integrationIds.map((id) => [id, expect.any(Object)]))
    );
    expect(composition.auctionContextRegistryForTest()?.snapshotInventoryForTest()).toEqual({
      disposed: false,
      registrations: ['permutive'],
    });
    expect(activeObservers.size).toBe(1);
    expect(activeMutationObservers.size).toBeGreaterThan(0);
    expect(activeCaptureListeners).toBe(1);

    window.dispatchEvent(new Event('resize'));
    composition.runtime.dispose();
    composition.runtime.dispose();
    await Promise.resolve();

    expect(events.filter((event) => event.startsWith('dispose:'))).toEqual(
      [...integrationIds].reverse().map((id) => `dispose:${id}`)
    );
    expect(activeObservers.size).toBe(0);
    expect(activeMutationObservers.size).toBe(0);
    expect(activeCaptureListeners).toBe(0);
    expect(composition.auctionContextRegistryForTest()).toBeUndefined();
    expect(vi.getTimerCount()).toBe(0);
    expect(Element.prototype.appendChild).toBe(appendChildBefore);
    expect(Element.prototype.insertBefore).toBe(insertBeforeBefore);
    expect(Object.getOwnPropertyDescriptor(window, 'fetch')).toEqual(fetchBefore);
    expect(Object.getOwnPropertyDescriptor(navigator, 'sendBeacon')).toEqual(sendBeaconBefore);
    expect(Object.getOwnPropertyDescriptor(window, 'didomiConfig')).toEqual(didomiBefore);
    expect(Object.getOwnPropertyDescriptor(window, 'testlight')).toEqual(testlightBefore);
  });

  it.each([
    {
      name: 'a real activation fails after acquiring its composed effects',
      failureId: 'permutive',
      phase: 'activate' as const,
    },
    {
      name: 'one real registration receives malformed frozen config',
      failureId: 'sourcepoint',
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
      const failureIndex = harness.integrationIds.indexOf(failureId);
      const preparedIds =
        phase === 'activate'
          ? harness.integrationIds
          : harness.integrationIds.slice(0, failureIndex + 1);
      const activatedIds =
        phase === 'activate' ? harness.integrationIds.slice(0, failureIndex + 1) : [];

      expect(installed).toEqual({ state: 'fallback', reason: 'bundle_partial' });
      expect(harness.composition.runtime.state).toBe('fallback');
      expect(harness.target['_internal']).toMatchObject({
        state: 'fallback',
        reason: 'bundle_partial',
      });
      expect(harness.events.filter((event) => event.startsWith('register:'))).toEqual(
        harness.integrationIds.map((id) => `register:${id}`)
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
        kind === 'storage' ? [{ id: 'sourcepoint', phase: 'after_commit' }] : [];

      expect(installed).toEqual({
        state: 'kernel',
        runtimeFailures: expectedRuntimeFailures,
        dispose: expect.any(Function),
      });
      expect(harness.composition.runtime.state).toBe('kernel');
      expect(harness.events.filter((event) => event.startsWith('prepare:'))).toEqual(
        harness.integrationIds.map((id) => `prepare:${id}`)
      );
      expect(harness.events.filter((event) => event.startsWith('activate:'))).toEqual(
        harness.integrationIds.map((id) => `activate:${id}`)
      );
      expect(
        harness.composition.auctionContextRegistryForTest()?.snapshotInventoryForTest()
      ).toEqual({ disposed: false, registrations: ['permutive'] });
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
      const disposedBeforeRuntimeRelease = kind === 'storage' ? ['dispose:sourcepoint'] : [];
      expect(harness.events.filter((event) => event.startsWith('dispose:'))).toEqual(
        disposedBeforeRuntimeRelease
      );
      await harness.assertReleased();
      const reverseIds = [...harness.integrationIds].reverse();
      const expectedDisposals =
        kind === 'storage'
          ? [
              'dispose:sourcepoint',
              ...reverseIds.filter((id) => id !== 'sourcepoint').map((id) => `dispose:${id}`),
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
