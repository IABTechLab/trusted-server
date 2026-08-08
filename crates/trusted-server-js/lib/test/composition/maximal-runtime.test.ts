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
  events: string[]
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
});
