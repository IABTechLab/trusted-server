import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  createBrowserGoogletagAdapter,
  createNoopGoogletagAdapter,
  type GoogletagAdapter,
  type GoogletagBindingStatus,
  type GoogletagDiagnosticsFact,
  type GoogletagDiagnosticsObserver,
  type GoogletagFacade,
  type GoogletagPublisherCallObserver,
  type GoogletagPublisherRefreshCall,
  type GptSlotTokenV1,
  type GptTraceCycleOrdinalV1,
} from '../../src/adapters/googletag';
import {
  createBrowserMessagingAdapter,
  createNoopMessagingAdapter,
  type CaptureMessageListener,
  type MessagingAdapter,
} from '../../src/adapters/messaging';
import {
  createNoopPrebidAdapter,
  PrebidAdmissionContractError,
  type PrebidAdapter,
  type PrebidBindingStatus,
  type PrebidEventFacade,
  type PrebidFacade,
  type PrebidTrustedServerAuctionV1,
  type PreparedTrustedBidV1,
} from '../../src/adapters/prebid';
import {
  BROWSER_TEST_DIAGNOSTICS_PROVIDER_ID,
  BROWSER_TEST_TRACE_PROVIDER_ID,
  createBrowserComposition,
  createNoopBrowserComposition,
  createTestBrowserRuntimeComposition,
} from '../../src/composition/browser_test';
import { log as localLog } from '../../src/core/log';
import {
  createDiagnosticsPresentationIntegrationRegistration,
  TRACE_PANEL_ID,
} from '../../src/integrations/gpt_diagnostics/presentation';
import type { BrowserAuctionBidV1, BrowserAuctionProjectionV1 } from '../../src/core/types';
import { createCreativeIntegrationRegistration as createProductionCreativeIntegrationRegistration } from '../../src/integrations/creative/module';
import { createDataDomeIntegrationRegistration } from '../../src/integrations/datadome/module';
import { createDidomiIntegrationRegistration } from '../../src/integrations/didomi/module';
import { createGoogleTagManagerIntegrationRegistration } from '../../src/integrations/google_tag_manager/module';
import { createLegacyGptRegistrationForTest as createGptIntegrationRegistration } from '../helpers/legacy_gpt_registration';
import { isGuardInstalled, resetGuardState } from '../../src/integrations/gpt/script_guard';
import { createGptDiagnosticsIntegrationRegistration } from '../../src/integrations/gpt_diagnostics/module';
import { createLockrIntegrationRegistration } from '../../src/integrations/lockr/module';
import { createOsanoIntegrationRegistration } from '../../src/integrations/osano/module';
import { createOsanoLifecycleIntegrationRegistration } from '../../src/integrations/osano/lifecycle';
import { createPermutiveIntegrationRegistration } from '../../src/integrations/permutive/module';
import { createPermutiveLifecycleIntegrationRegistration } from '../../src/integrations/permutive/lifecycle';
import { createSourcepointIntegrationRegistration } from '../../src/integrations/sourcepoint/module';
import { createSourcepointLifecycleIntegrationRegistration } from '../../src/integrations/sourcepoint/lifecycle';
import { createTestlightIntegrationRegistration } from '../../src/integrations/testlight/module';
import { createRenderRuntimeIntegrationRegistration } from '../../src/integrations/render_runtime/module';
import { publicLog } from '../../src/kernel/fallback';
import { createTestNavigationIdentityIssuer } from '../../src/kernel/identity';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
  PreparedIntegration,
} from '../../src/kernel/integration_registry';
import { RELEASE_CATALOG } from '../../src/kernel/release_catalog';
import {
  createRenderAttempt,
  type CommittedRenderArtifact,
  type RenderAttempt,
} from '../../src/services/render';

const DEFERRED_INTEGRATION_IDS = new Set([
  'diagnostics_presentation',
  'gpt_later',
  'osano_lifecycle',
  'permutive_lifecycle',
  'prebid_later',
  'sourcepoint_lifecycle',
]);

const GPT_DIAGNOSTICS_TEST_IDS = Object.freeze([
  BROWSER_TEST_DIAGNOSTICS_PROVIDER_ID,
  'gpt_diagnostics',
  'diagnostics_presentation',
]);

const BROWSER_TEST_OPTIONAL_GPT_DIAG_PROVIDER_ID = 'browser_test_optional_gpt_diag_provider';

function runtimeManifest(releaseId: string, ids: readonly string[]) {
  return {
    version: 1 as const,
    releaseId,
    firstDisplay: null,
    runtimeSrc: `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`,
    integrations: ids.map((id) =>
      DEFERRED_INTEGRATION_IDS.has(id)
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

function runtimeCatalog(ids: readonly string[]) {
  return Object.freeze(
    ids.map((id) => {
      const canonical = RELEASE_CATALOG.find((entry) => entry.id === id);
      if (canonical) {
        return Object.freeze({
          id,
          phase: canonical.phase,
          trigger: canonical.trigger,
          config: canonical.config,
          consumes: canonical.consumes,
          provides: canonical.provides,
        });
      }
      return Object.freeze({
        id,
        phase: DEFERRED_INTEGRATION_IDS.has(id) ? ('deferred' as const) : ('takeover' as const),
        trigger: DEFERRED_INTEGRATION_IDS.has(id) ? ('first_display_or_idle' as const) : null,
        config: null,
        consumes: Object.freeze(
          id === BROWSER_TEST_DIAGNOSTICS_PROVIDER_ID
            ? ['runtime.v1']
            : id === 'gpt_diagnostics'
              ? ['runtime.v1', 'gpt.events.v1']
              : id === 'diagnostics_presentation'
                ? ['runtime.v1', 'trace.presentation.v1', 'gpt_diag.v1?gpt_diagnostics_active']
                : []
        ),
        provides: Object.freeze(
          id === BROWSER_TEST_DIAGNOSTICS_PROVIDER_ID
            ? ['gpt.events.v1', 'trace.v1', 'trace.presentation.v1']
            : id === BROWSER_TEST_TRACE_PROVIDER_ID
              ? ['trace.v1', 'trace.presentation.v1']
              : id === 'gpt_diagnostics' || id === BROWSER_TEST_OPTIONAL_GPT_DIAG_PROVIDER_ID
                ? ['gpt_diag.v1']
                : []
        ),
      });
    })
  );
}

function exactLegacyRuntime(
  interfaces: Readonly<Record<string, unknown>>,
  id: 'creative' | 'prebid'
): Readonly<{ activate: (config?: unknown) => () => void; start: (config: unknown) => void }> {
  const runtime = interfaces[id] as Readonly<{ activate?: unknown; start?: unknown }> | undefined;
  if (
    !runtime ||
    !Object.isFrozen(runtime) ||
    typeof runtime.activate !== 'function' ||
    typeof runtime.start !== 'function'
  ) {
    throw new TypeError(`${id} test runtime is unavailable`);
  }
  return runtime as Readonly<{
    activate: (config?: unknown) => () => void;
    start: (config: unknown) => void;
  }>;
}

function testTakeoverRegistration(
  id: string,
  releaseId: string,
  prepare: (
    context: IntegrationPrepareContext
  ) => PreparedIntegration | PromiseLike<PreparedIntegration>
): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id,
    phase: 'takeover',
    releaseId,
    prepareSync: (context: IntegrationPrepareContext) => prepare(context) as PreparedIntegration,
    prepare,
  });
}

function createLegacyPrebidIntegrationRegistration(releaseId: string): IntegrationRegistration {
  const prepare = ({ config, interfaces }: IntegrationPrepareContext) => {
    const runtime = exactLegacyRuntime(interfaces, 'prebid');
    return Object.freeze({
      activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
        const release = runtime.activate();
        onDispose(release);
        afterCommit(() => runtime.start(config));
      },
    });
  };
  return Object.freeze({
    abi: 1,
    id: 'prebid',
    phase: 'takeover',
    releaseId,
    prepareSync: prepare,
    prepare,
  });
}

function createLegacyCreativeIntegrationRegistration(releaseId: string): IntegrationRegistration {
  const prepare = ({ config, interfaces }: IntegrationPrepareContext) => {
    const creative = config as Readonly<{
      clickGuard?: unknown;
      enabled?: unknown;
      renderGuard?: unknown;
    }>;
    if (!creative.enabled || (!creative.clickGuard && !creative.renderGuard)) {
      return Object.freeze({ activate: () => undefined });
    }
    const runtime = exactLegacyRuntime(interfaces, 'creative');
    return Object.freeze({
      activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
        const release = runtime.activate(config);
        onDispose(release);
        afterCommit(() => runtime.start(config));
      },
    });
  };
  return Object.freeze({
    abi: 1,
    id: 'creative',
    phase: 'takeover',
    releaseId,
    prepareSync: prepare,
    prepare,
  });
}

function createTarget() {
  return {
    googletag: undefined as unknown,
    pbjs: undefined as unknown,
    addEventListener:
      vi.fn<(type: 'message', listener: CaptureMessageListener, capture: true) => void>(),
    removeEventListener:
      vi.fn<(type: 'message', listener: CaptureMessageListener, capture: true) => void>(),
  };
}

function browserSlotPlacement(slot: string, divId = slot) {
  return Object.freeze({
    slot,
    gamUnitPath: `/123/${slot}`,
    divId,
    formats: Object.freeze([Object.freeze([300, 250] as const)]),
    targeting: Object.freeze({}),
  });
}

function fakeGoogletagAdapter(
  bindingStatus: () => GoogletagBindingStatus = () => 'pending'
): GoogletagAdapter {
  return Object.freeze({ ...createNoopGoogletagAdapter(), bindingStatus });
}

function synchronousGptAdapter(initialSlots: readonly object[] = []) {
  type Listener = Readonly<{
    callback: Parameters<GoogletagFacade['subscribe']>[1];
    diagnosticsOwner: boolean;
  }>;
  const listeners = new Map<string, Set<Listener>>();
  const physicalSlots: object[] = [...initialSlots];
  const targeting = new WeakMap<object, Map<string, readonly string[]>>();
  const bindingToken = Object.freeze({});
  const display = vi.fn();
  const refresh = vi.fn();
  const diagnosticsSlots = new WeakMap<object, GoogletagDiagnosticsFact['slot']>();
  const diagnosticFacts: GoogletagDiagnosticsFact[] = [];
  const traceTokens = new WeakMap<object, GptSlotTokenV1>();
  const traceCycles = new WeakMap<object, GptTraceCycleOrdinalV1>();
  let traceTokenSequence = 0;
  let diagnosticsObserver: GoogletagDiagnosticsObserver | undefined;
  let publisherObserver: GoogletagPublisherCallObserver | undefined;
  const traceTokenFor = (slot: object): GptSlotTokenV1 => {
    const existing = traceTokens.get(slot);
    if (existing) return existing;
    const token = `gt1_${(++traceTokenSequence).toString(36)}` as GptSlotTokenV1;
    traceTokens.set(slot, token);
    return token;
  };
  const transactionalDefine: GoogletagFacade['transactionalDefine'] = (
    definition,
    isGenerationCurrent,
    prepareCommit
  ) => {
    if (!isGenerationCurrent()) return Object.freeze({ status: 'discarded' as const });
    const slot = {
      addService: vi.fn(),
      getAdUnitPath: () => definition.adUnitPath,
      getSlotElementId: () => definition.elementId,
    };
    const admission = prepareCommit(slot);
    if (!admission.commit() || !isGenerationCurrent()) {
      admission.rollback();
      return Object.freeze({ status: 'discarded' as const });
    }
    physicalSlots.push(slot);
    return Object.freeze({ status: 'defined' as const, slot });
  };
  const facade: GoogletagFacade = Object.freeze({
    adUnitPath: (slot: object) =>
      'getAdUnitPath' in slot && typeof slot.getAdUnitPath === 'function'
        ? slot.getAdUnitPath()
        : undefined,
    bindingToken: () => bindingToken,
    clearTargeting: vi.fn((slot: object, key?: string) => {
      const values = targeting.get(slot);
      if (key === undefined) values?.clear();
      else values?.delete(key);
    }),
    transactionalDefine,
    display,
    getTargeting: vi.fn((slot: object, key: string) =>
      Object.freeze([...(targeting.get(slot)?.get(key) ?? [])])
    ),
    observeTargeting: () => Object.assign(vi.fn(), { isCurrent: () => true }),
    refresh,
    serviceState: () =>
      Object.freeze({ apiReady: true, initialLoadDisabled: false, pubadsReady: true }),
    setTargeting: vi.fn((slot: object, key: string, value: string | readonly string[]) => {
      const values = targeting.get(slot) ?? new Map<string, readonly string[]>();
      targeting.set(slot, values);
      values.set(key, Object.freeze(typeof value === 'string' ? [value] : [...value]));
    }),
    slotElementId: (slot: object) =>
      'getSlotElementId' in slot && typeof slot.getSlotElementId === 'function'
        ? slot.getSlotElementId()
        : undefined,
    slots: () => Object.freeze([...physicalSlots]),
    subscribe: (
      eventType: string,
      listener: Parameters<GoogletagFacade['subscribe']>[1],
      diagnosticsOwner = false
    ) => {
      const registered = listeners.get(eventType) ?? new Set();
      const entry = Object.freeze({ callback: listener, diagnosticsOwner });
      registered.add(entry);
      listeners.set(eventType, registered);
      return () => registered.delete(entry);
    },
    transactionalReplace: () => Object.freeze({ status: 'destroyed' as const }),
  });
  const adapter: GoogletagAdapter = Object.freeze({
    bindingStatus: () => 'present',
    diagnosticsIdentity: () => undefined,
    dispose: vi.fn(),
    notifyReady: vi.fn(),
    observeDiagnostics: (observer: GoogletagDiagnosticsObserver) => {
      if (diagnosticsObserver) return undefined;
      diagnosticsObserver = observer;
      return () => {
        if (diagnosticsObserver === observer) diagnosticsObserver = undefined;
      };
    },
    observePublisherCalls: (observer: GoogletagPublisherCallObserver) => {
      publisherObserver = observer;
      return () => {
        if (publisherObserver === observer) publisherObserver = undefined;
      };
    },
    run: <Value>(command: (gpt: Readonly<GoogletagFacade>) => Value) => {
      let result: Promise<Value>;
      try {
        result = Promise.resolve(command(facade));
      } catch (error) {
        result = Promise.reject(error);
      }
      return Object.freeze({ status: 'present' as const, result, dispose: vi.fn() });
    },
    traceToken: traceTokenFor,
  });
  return {
    adapter,
    emit: (eventType: string, event: unknown): void => {
      let acceptedHandle: unknown;
      const publishFact = (handle: unknown): void => {
        if (typeof event !== 'object' || event === null || !('slot' in event)) return;
        const physicalSlot = event.slot;
        if (typeof physicalSlot !== 'object' || physicalSlot === null) return;
        let safeSlot = diagnosticsSlots.get(physicalSlot);
        if (!safeSlot) {
          const elementId =
            'getSlotElementId' in physicalSlot &&
            typeof physicalSlot.getSlotElementId === 'function'
              ? physicalSlot.getSlotElementId()
              : undefined;
          const adUnitPath =
            'getAdUnitPath' in physicalSlot && typeof physicalSlot.getAdUnitPath === 'function'
              ? physicalSlot.getAdUnitPath()
              : undefined;
          const createdSlot = Object.freeze({
            token: Object.freeze(Object.create(null) as object),
            traceToken: traceTokenFor(physicalSlot),
            ...(typeof elementId === 'string' ? { elementId } : {}),
            ...(typeof adUnitPath === 'string' ? { adUnitPath } : {}),
          });
          diagnosticsSlots.set(physicalSlot, createdSlot);
          safeSlot = createdSlot;
        }
        if (eventType === 'slotRequested' && handle !== undefined) {
          const next = ((traceCycles.get(physicalSlot) ?? 0) + 1) as GptTraceCycleOrdinalV1;
          traceCycles.set(physicalSlot, next);
        }
        const cycleOrdinal = traceCycles.get(physicalSlot);
        const fact = Object.freeze({
          ...event,
          kind: eventType,
          observedAtMs: 1,
          slot: Object.freeze({
            ...safeSlot,
            ...(cycleOrdinal === undefined ? {} : { cycleOrdinal }),
          }),
        }) as Parameters<GoogletagDiagnosticsObserver>[0];
        diagnosticFacts.push(fact);
        diagnosticsObserver?.(fact);
      };
      for (const listener of listeners.get(eventType) ?? []) {
        const handle = listener.callback(event);
        if (!listener.diagnosticsOwner) {
          if (handle !== undefined) acceptedHandle = handle;
          if (eventType === 'slotRequested' || eventType === 'slotRenderEnded') {
            publishFact(handle);
          }
          continue;
        }
        publishFact(acceptedHandle);
      }
    },
    diagnosticsObserverActive: () => diagnosticsObserver !== undefined,
    diagnosticFacts: () => Object.freeze([...diagnosticFacts]),
    display,
    listenerInventory: () =>
      Object.freeze(
        [...listeners.entries()]
          .filter(([, registered]) => registered.size > 0)
          .map(([eventType, registered]) => Object.freeze([eventType, registered.size] as const))
      ),
    listenerRoles: (eventType: string) =>
      Object.freeze(
        [...(listeners.get(eventType) ?? [])].map((listener) => listener.diagnosticsOwner)
      ),
    publisherRefresh: (call: Readonly<GoogletagPublisherRefreshCall>) => {
      const observer = publisherObserver;
      if (!observer?.refresh) throw new Error('Publisher observer is unavailable');
      return observer.refresh(call);
    },
    physicalSlots: () => Object.freeze([...physicalSlots]),
    refresh,
    targetingFor: (slot: object) => new Map(targeting.get(slot) ?? []),
  };
}

function fakePrebidAdapter(
  bindingStatus: () => PrebidBindingStatus = () => 'pending'
): PrebidAdapter {
  return Object.freeze({ ...createNoopPrebidAdapter(), bindingStatus });
}

function synchronousPrebidAdapter(
  admission: (prepared: Readonly<PreparedTrustedBidV1>) => 'admitted' | 'not_admitted' = () =>
    'admitted'
) {
  let auctionListener: ((auction: Readonly<PrebidTrustedServerAuctionV1>) => void) | undefined;
  let auctionEndListener:
    ((event: unknown, prebid: Readonly<PrebidEventFacade>) => void) | undefined;
  let admitted: Readonly<PreparedTrustedBidV1> | undefined;
  const admitTrustedBid = vi.fn((prepared: Readonly<PreparedTrustedBidV1>) => {
    const result = admission(prepared);
    if (result === 'admitted') admitted = prepared;
    return result;
  });
  const requestBids = vi.fn();
  const setTargetingForGpt = vi.fn();
  const facade = Object.freeze({
    addAdUnits: vi.fn(),
    highestBids: vi.fn(() => Object.freeze([])),
    processQueue: vi.fn(),
    registerBidAdapter: vi.fn(),
    registerTrustedServerBidder: vi.fn(
      (listener: (auction: Readonly<PrebidTrustedServerAuctionV1>) => void) => {
        auctionListener = listener;
        return () => {
          auctionListener = undefined;
        };
      }
    ),
    renderAd: vi.fn(),
    requestBids,
    setTargetingForGpt,
    subscribe: vi.fn(
      (
        eventType: string,
        listener: (event: unknown, prebid: Readonly<PrebidEventFacade>) => void
      ) => {
        if (eventType === 'auctionEnd') auctionEndListener = listener;
        return () => {
          if (auctionEndListener === listener) auctionEndListener = undefined;
        };
      }
    ),
  }) satisfies PrebidFacade;
  const adapter = Object.freeze({
    ...createNoopPrebidAdapter(),
    admitTrustedBid,
    bindingStatus: () => 'present' as const,
    run: <Value>(command: (prebid: Readonly<PrebidFacade>) => Value) => {
      let result: Promise<Value>;
      try {
        result = Promise.resolve(command(facade));
      } catch (error) {
        result = Promise.reject(error);
      }
      return Object.freeze({ status: 'present' as const, result, dispose: vi.fn() });
    },
  }) satisfies PrebidAdapter;
  return {
    adapter,
    admitTrustedBid,
    auction: (auction: Readonly<PrebidTrustedServerAuctionV1>): void => auctionListener?.(auction),
    auctionEnd: (auctionId: string): void => {
      const prepared = admitted;
      const highest = prepared
        ? Object.freeze([
            Object.freeze({
              ...prepared.bid,
              adUnitCode: prepared.adUnitCode,
              auctionId: prepared.auctionId,
            }),
          ])
        : Object.freeze([]);
      auctionEndListener?.(
        Object.freeze({ auctionId }),
        Object.freeze({ highestBids: () => highest })
      );
    },
    requestBids,
    setTargetingForGpt,
  };
}

function fakeMessagingAdapter(
  installCaptureListener: MessagingAdapter['installCaptureListener'] = () => vi.fn()
): MessagingAdapter {
  return Object.freeze({ ...createNoopMessagingAdapter(), installCaptureListener });
}

describe('browser composition', () => {
  afterEach(() => {
    vi.useRealTimers();
    document.head.querySelectorAll('script#trustedserver-js').forEach((script) => script.remove());
    Object.defineProperty(document, 'currentScript', { configurable: true, value: null });
  });

  it('constructs live adapters without changing production globals', () => {
    const target = createTarget();
    const composition = createBrowserComposition({ target });

    expect(composition.adapters.googletag.bindingStatus()).toBe('pending');
    expect(composition.adapters.prebid.bindingStatus()).toBe('pending');
    expect(target.addEventListener).not.toHaveBeenCalled();

    target.googletag = {};
    target.pbjs = {};
    expect(composition.adapters.googletag.bindingStatus()).toBe('incompatible');
    expect(composition.adapters.prebid.bindingStatus()).toBe('incompatible');

    target.googletag = 1;
    target.pbjs = 'not-prebid';
    expect(composition.adapters.googletag.bindingStatus()).toBe('incompatible');
    expect(composition.adapters.prebid.bindingStatus()).toBe('incompatible');
  });

  it('routes the first-display measure through the concrete test composition', async () => {
    const display = vi.fn();
    const pubadsService = {};
    const performance = { mark: vi.fn(), measure: vi.fn() };
    const target = {
      ...createTarget(),
      googletag: {
        apiReady: true,
        cmd: {
          push: (command: () => void): number => {
            command();
            return 1;
          },
        },
        display,
        pubads: vi.fn(() => pubadsService),
      },
      performance,
    };
    const composition = createBrowserComposition({ target });

    target.googletag.display('publisher-slot');
    expect(performance.mark).not.toHaveBeenCalled();

    await expect(
      composition.adapters.googletag.run((gpt) => {
        gpt.display('authoritative-slot');
        gpt.display('replay-slot');
      }).result
    ).resolves.toBeUndefined();

    expect(performance.mark).toHaveBeenCalledExactlyOnceWith('tsjs:first-display');
    expect(performance.measure).toHaveBeenCalledExactlyOnceWith(
      'tsjs:boot-to-first-display',
      'tsjs:bids-script',
      'tsjs:first-display'
    );
    expect(display).toHaveBeenCalledTimes(3);
  });

  it('routes an attributable empty GPT cycle through the owned slot and PUC services', async () => {
    const gpt = synchronousGptAdapter();
    let prefix = 0;
    const reservationId = `r1_${'a'.repeat(22)}`;
    const source = Object.freeze({
      type: 'adm' as const,
      version: 1 as const,
      adm: '<main>fictional fallback</main>',
      width: 300,
      height: 250,
    });
    const bid = Object.freeze({
      candidateId: 'AAAAAAAAAAAA',
      slot: 'slot-one',
      provider: 'trusted',
      upstreamBidId: 'upstream-one',
      cpm: 1,
      currency: 'USD' as const,
      targeting: Object.freeze({ hb_bidder: 'trusted' }),
      rendererReservationId: reservationId,
      renderSource: source,
    });
    const projection = Object.freeze({
      version: 1,
      auction: Object.freeze({
        version: 1,
        auctionId: 'initial',
        results: Object.freeze([
          Object.freeze({
            slot: 'slot-one',
            outcome: 'winner' as const,
            candidateId: bid.candidateId,
          }),
        ]),
      }),
      slots: Object.freeze([browserSlotPlacement('slot-one')]),
      bids: Object.freeze([bid]),
    });
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: runtimeManifest('a'.repeat(64), []),
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: projection,
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: gpt.adapter,
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        createIdentityIssuerForTest: () => {
          prefix += 1;
          return createTestNavigationIdentityIssuer({
            getRandomValues: (target) => {
              target.fill(prefix);
              return target;
            },
          });
        },
      }
    );
    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });

    const session = composition.runtimeSessionForTest();
    const navigation = session?.currentNavigation;
    const batch = navigation?.createAuctionBatch('gpt-primary');
    const services = session?.interfaces;
    const artifacts = services?.['artifacts'];
    const reservations = composition.reservationServiceForTest();
    const slots = composition.slotServiceForTest();
    if (!navigation || !batch || !artifacts || !reservations || !slots) {
      throw new Error('Expected runtime-owned GPT dependencies');
    }
    const createAttempt = (parentAttemptId?: string): RenderAttempt => {
      const owner = batch.createRenderAttempt('slot-one');
      if (!owner.ok) throw new Error(owner.reason);
      const attempt = createRenderAttempt({
        artifacts: artifacts as Parameters<typeof createRenderAttempt>[0]['artifacts'],
        owner: owner.value,
        prepareRenderSource: (candidate) =>
          typeof candidate === 'object' && candidate !== null && Object.isFrozen(candidate)
            ? (candidate as Readonly<{ type: 'aps' | 'adm'; version: 1 }>)
            : undefined,
        reservations,
        ...(parentAttemptId === undefined ? {} : { parentAttemptId }),
      });
      if (!attempt.ok) throw new Error(attempt.reason);
      return attempt.value;
    };
    const ownerResult = batch.createRenderAttempt('slot-one');
    if (!ownerResult.ok) throw new Error(ownerResult.reason);
    const primaryResult = createRenderAttempt({
      artifacts: artifacts as Parameters<typeof createRenderAttempt>[0]['artifacts'],
      owner: ownerResult.value,
      prepareRenderSource: () => source,
      reservations,
    });
    if (!primaryResult.ok) throw new Error(primaryResult.reason);
    const primary = primaryResult.value;
    const physicalSlot = Object.freeze({});
    const slotElement = document.createElement('div');
    slotElement.id = 'slot-one';
    document.body.append(slotElement);
    expect(
      slots.adoptGptSlot(navigation.generation, 'slot-one', {
        definition: {
          adUnitPath: '/123/slot-one',
          elementId: 'slot-one',
          sizes: Object.freeze([[300, 250]]),
        },
        ownership: 'trusted_server',
        slot: physicalSlot,
      })
    ).toEqual({ ok: true });
    const artifact = Object.freeze({
      kind: 'puc' as const,
      attemptId: primary.id,
      slot: primary.slot,
      navigationGeneration: primary.navigationGeneration,
      dispose: vi.fn(),
    }) satisfies CommittedRenderArtifact;
    const projectedBid = (
      navigation.currentAuctionProjection as Readonly<{ bids: readonly BrowserAuctionBidV1[] }>
    ).bids[0];
    if (!projectedBid || !('rendererReservationId' in projectedBid)) {
      throw new Error('Expected the parsed owned projected winner');
    }
    const projectedPlacement = (
      navigation.currentAuctionProjection as Readonly<Pick<BrowserAuctionProjectionV1, 'slots'>>
    ).slots[0];
    if (!projectedPlacement) throw new Error('Expected the parsed projected placement');
    let fallback: RenderAttempt | undefined;
    const operation = await composition.publishGptWinnerForTest({
      artifact,
      attempt: primary,
      bid: projectedBid,
      createFallback: (parentAttemptId) => {
        fallback = createAttempt(parentAttemptId);
        return Object.freeze({ ok: true as const, value: fallback });
      },
      operation: 'refresh',
      owner: ownerResult.value,
      placement: projectedPlacement,
      requestClass: 'primary',
      slot: physicalSlot,
    });
    expect(operation.ok).toBe(true);

    await Promise.resolve();
    await Promise.resolve();
    expect(gpt.refresh).toHaveBeenCalledExactlyOnceWith(
      [physicalSlot],
      Object.freeze({ changeCorrelator: false })
    );
    gpt.emit('slotRequested', { slot: physicalSlot });
    gpt.emit('slotRenderEnded', {
      isEmpty: true,
      responseIdentifier: 'response-one',
      slot: physicalSlot,
    });
    await Promise.resolve();

    expect(primary.snapshot().outcome).toEqual({ outcome: 'failed', reason: 'gam_empty' });
    expect(fallback).toBeDefined();
    fallback?.fail('winner_not_renderable');
    expect(operation.ok && operation.value.snapshot()).toMatchObject({
      settled: true,
      result: {
        path: 'fallback',
        primary: { outcome: 'failed', reason: 'gam_empty' },
        fallback: { outcome: 'failed', reason: 'winner_not_renderable' },
      },
    });
    composition.runtime.dispose();
    slotElement.remove();
  });

  it('publishes the accepted initial projection through the production GPT lifecycle', async () => {
    const releaseId = 'a'.repeat(64);
    const gpt = synchronousGptAdapter();
    const placement = browserSlotPlacement('initial-slot');
    const bid = Object.freeze({
      candidateId: 'AAAAAAAAAAAA',
      slot: placement.slot,
      provider: 'trusted',
      upstreamBidId: 'initial-upstream',
      cpm: 1.5,
      currency: 'USD' as const,
      targeting: Object.freeze({ hb_bidder: 'trusted', pos: 'bid' }),
      rendererReservationId: `r1_${'i'.repeat(22)}`,
      renderSource: Object.freeze({
        type: 'adm' as const,
        version: 1 as const,
        adm: '<main>initial winner</main>',
        width: 300,
        height: 250,
      }),
    });
    const projection = {
      version: 1,
      auction: {
        version: 1,
        auctionId: 'initial-production',
        results: [
          { slot: placement.slot, outcome: 'winner' as const, candidateId: bid.candidateId },
        ],
      },
      slots: [{ ...placement, targeting: { pos: 'placement', section: 'news' } }],
      bids: [bid],
    };
    const element = document.createElement('div');
    element.id = placement.divId;
    document.body.append(element);
    let prefix = 0;
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, ['gpt']),
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: {
          auctionProjection: projection,
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: gpt.adapter,
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        createIdentityIssuerForTest: () => {
          prefix += 1;
          return createTestNavigationIdentityIssuer({
            getRandomValues: (target) => {
              target.fill(prefix);
              return target;
            },
          });
        },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      await vi.waitFor(() => expect(gpt.physicalSlots()).toHaveLength(1));
      const physicalSlot = gpt.physicalSlots()[0];
      expect(physicalSlot).toBeDefined();
      expect(gpt.display).toHaveBeenCalledExactlyOnceWith(physicalSlot);
      expect(gpt.refresh).not.toHaveBeenCalled();
      expect(gpt.targetingFor(physicalSlot!)).toEqual(
        new Map([
          ['hb_adid', [bid.rendererReservationId]],
          ['hb_bidder', ['trusted']],
          ['pos', ['bid']],
          ['section', ['news']],
        ])
      );
      gpt.emit('slotRequested', { slot: physicalSlot });
      gpt.emit('slotRenderEnded', {
        isEmpty: true,
        responseIdentifier: 'initial-empty-response',
        slot: physicalSlot,
      });
      await vi.waitFor(() => expect(element.querySelector('iframe')).not.toBeNull());
    } finally {
      composition.runtime.dispose();
      element.remove();
    }
  });

  it('reuses one publisher GPT slot resolved through a unique responsive DOM prefix', async () => {
    const releaseId = 'a'.repeat(64);
    const publisherSlot = {
      getAdUnitPath: () => '/publisher/existing',
      getSlotElementId: () => 'responsive-mobile',
    };
    const gpt = synchronousGptAdapter([publisherSlot]);
    const placement = {
      slot: 'responsive-slot',
      gamUnitPath: '/123/responsive-slot',
      divId: 'responsive-',
      formats: [[300, 250]],
      targeting: {},
    };
    const bid = {
      candidateId: 'CCCCCCCCCCCC',
      slot: placement.slot,
      provider: 'trusted',
      upstreamBidId: 'responsive-upstream',
      cpm: 1,
      currency: 'USD' as const,
      targeting: {},
      rendererReservationId: `r1_${'r'.repeat(22)}`,
      renderSource: {
        type: 'adm' as const,
        version: 1 as const,
        adm: '<main>responsive winner</main>',
        width: 300,
        height: 250,
      },
    };
    const element = document.createElement('div');
    element.id = 'responsive-mobile';
    document.body.append(element);
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, ['gpt']),
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: {
              version: 1,
              auctionId: 'responsive-initial',
              results: [
                { slot: placement.slot, outcome: 'winner' as const, candidateId: bid.candidateId },
              ],
            },
            slots: [placement],
            bids: [bid],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: gpt.adapter,
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        createIdentityIssuerForTest: () =>
          createTestNavigationIdentityIssuer({
            getRandomValues: (target) => {
              target.fill(7);
              return target;
            },
          }),
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      await vi.waitFor(() => expect(gpt.refresh).toHaveBeenCalledOnce());
      expect(gpt.physicalSlots()).toEqual([publisherSlot]);
      expect(gpt.display).not.toHaveBeenCalled();
      expect(gpt.refresh).toHaveBeenCalledExactlyOnceWith(
        [publisherSlot],
        Object.freeze({ changeCorrelator: false })
      );
    } finally {
      composition.runtime.dispose();
      element.remove();
    }
  });

  it('derives exact APS validation coordinates only for the real browser target', () => {
    const renderer = {
      type: 'aps',
      version: 1,
      accountId: 'publisher-account',
      bidId: 'bid-1',
      tagType: 'iframe',
      creativeUrl: 'https://creative.example/render',
      width: 300,
      height: 250,
      aaxResponse: 'renderer-envelope',
    };
    const message = {
      message: 'TS APS Start',
      version: 1,
      lifecycleTicket: 't1_abcdefghijklmnopqrstuv',
      rendererUrl: new URL('/integrations/aps/renderer/v1', window.location.origin).href,
      envelope: {
        version: 1,
        nonce: 'n1_abcdefghijklmnopqrstuv',
        publisherOrigin: window.location.origin,
        renderer,
      },
    };
    const validation = { validateApsRenderer: () => true };

    const browser = createBrowserComposition({ messagingValidation: validation });
    expect(browser.adapters.messaging.parseProtocolMessage('apsStart', message)).toBeDefined();

    const injected = createBrowserComposition({
      target: createTarget(),
      messagingValidation: validation,
    });
    expect(injected.adapters.messaging.parseProtocolMessage('apsStart', message)).toBeUndefined();
  });

  it('installs the capture-phase message listener synchronously and disposes once', () => {
    const target = createTarget();
    const composition = createBrowserComposition({ target });
    const listener = vi.fn();

    const dispose = composition.adapters.messaging.installCaptureListener(listener);
    expect(dispose).toBeTypeOf('function');

    expect(target.addEventListener).toHaveBeenCalledTimes(1);
    const installed = target.addEventListener.mock.calls[0]?.[1];
    expect(installed).toBeTypeOf('function');
    expect(target.addEventListener).toHaveBeenCalledWith('message', installed, true);

    dispose?.();
    dispose?.();
    expect(target.removeEventListener).toHaveBeenCalledTimes(1);
    expect(target.removeEventListener).toHaveBeenCalledWith('message', installed, true);
  });

  it('uses exact injected fakes without constructing concrete adapters', () => {
    const googletag = fakeGoogletagAdapter(() => 'present');
    const prebid = fakePrebidAdapter(() => 'incompatible');
    const messaging = fakeMessagingAdapter();

    const composition = createBrowserComposition({
      adapters: { googletag, messaging, prebid },
    });

    expect(composition.adapters).toEqual({ googletag, messaging, prebid });
    expect(Object.isFrozen(composition.adapters)).toBe(true);
    expect(Object.isFrozen(composition)).toBe(true);
  });

  it('provides a side-effect-free no-op composition for kernel and service tests', () => {
    const composition = createNoopBrowserComposition();
    const listener = vi.fn();

    expect(composition.adapters.googletag.bindingStatus()).toBe('pending');
    expect(composition.adapters.prebid.bindingStatus()).toBe('pending');
    const disposeMessaging = composition.adapters.messaging.installCaptureListener(listener);
    expect(disposeMessaging).toBeUndefined();
    expect(listener).not.toHaveBeenCalled();
  });

  it('classifies exactly the six deferred integration IDs without a suffix heuristic', () => {
    const releaseId = 'a'.repeat(64);
    const deferredIds = Object.freeze([
      'diagnostics_presentation',
      'gpt_later',
      'osano_lifecycle',
      'permutive_lifecycle',
      'prebid_later',
      'sourcepoint_lifecycle',
    ]);
    const rows = runtimeManifest(releaseId, [
      'takeover_lifecycle',
      ...deferredIds,
      'later_takeover',
    ]).integrations;

    expect(rows.map(({ id, phase }) => [id, phase])).toEqual([
      ['takeover_lifecycle', 'takeover'],
      ...deferredIds.map((id) => [id, 'deferred']),
      ['later_takeover', 'takeover'],
    ]);
  });

  it.each([false, true])(
    'installs only the active GPT diagnostics fact path when boot active is %s',
    async (active) => {
      const releaseId = 'a'.repeat(64);
      const target: Record<string, unknown> = {};
      const gpt = synchronousGptAdapter();
      const integrationIds = active ? GPT_DIAGNOSTICS_TEST_IDS : Object.freeze([]);
      const composition = createTestBrowserRuntimeComposition(
        {
          target,
          releaseId,
          manifest: runtimeManifest(releaseId, integrationIds),
          knownIntegrationIds: integrationIds,
          catalog: runtimeCatalog(integrationIds),
          boot: {
            auctionProjection: {
              version: 1,
              auction: { version: 1, auctionId: 'boot', results: [] },
              slots: [],
              bids: [],
            },
            creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
            diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active } },
          },
          kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
        },
        {
          adapters: {
            googletag: gpt.adapter,
            messaging: fakeMessagingAdapter(),
            prebid: fakePrebidAdapter(),
          },
          coreActivations: { correctnessGptListeners: vi.fn() },
        }
      );

      expect(composition.runtime.start()).toBe(true);
      if (active) {
        expect(
          composition.runtime.registerIntegration(
            composition.createDiagnosticsCapabilityProviderRegistrationForTest()
          )
        ).toBe(true);
        expect(
          composition.runtime.registerIntegration(
            createGptDiagnosticsIntegrationRegistration(releaseId)
          )
        ).toBe(true);
      }
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });

      const inventory = Object.fromEntries(gpt.listenerInventory());
      expect(inventory).toEqual(
        active
          ? {
              impressionViewable: 1,
              slotOnload: 1,
              slotRenderEnded: 1,
              slotRequested: 1,
              slotResponseReceived: 1,
              slotVisibilityChanged: 1,
            }
          : { slotRenderEnded: 1, slotRequested: 1 }
      );
      expect(gpt.diagnosticsObserverActive()).toBe(active);
      const diagnostics = target['diagnostics'] as
        { readonly gpt?: { snapshot(): { slots: readonly unknown[] } } } | undefined;
      expect(Reflect.ownKeys(diagnostics ?? {}).sort()).toEqual(
        active ? ['gpt', 'renderTrace'] : ['renderTrace']
      );
      expect(target).not.toHaveProperty('gpt.events.v1');
      expect(target).not.toHaveProperty('trace.v1');
      expect(target).not.toHaveProperty('gpt_diag.v1');

      if (active) {
        const observedSlot = Object.freeze({
          getSlotElementId: () => 'diagnostic-slot',
          getAdUnitPath: () => '/diagnostic/slot',
        });
        gpt.emit('slotRequested', { slot: observedSlot });
        gpt.emit('slotResponseReceived', { slot: observedSlot });
        gpt.emit('slotRenderEnded', { slot: observedSlot, isEmpty: false, size: [300, 250] });
        expect(diagnostics?.gpt?.snapshot().slots).toHaveLength(1);
      }

      composition.runtime.dispose();
      expect(gpt.diagnosticsObserverActive()).toBe(false);
    }
  );

  it('composes the production GPT adapter lifecycle handle through SlotService and ingress into trace', async () => {
    const releaseId = 'a'.repeat(64);
    const listeners = new Map<string, Set<(event: unknown) => void>>();
    const refresh = vi.fn();
    const pubads = {
      addEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
        const registered = listeners.get(type) ?? new Set();
        registered.add(listener);
        listeners.set(type, registered);
      }),
      getSlots: vi.fn(() => [] as object[]),
      refresh,
      removeEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
        listeners.get(type)?.delete(listener);
      }),
    };
    const concreteAdapter = createBrowserGoogletagAdapter({
      googletag: {
        apiReady: true,
        pubadsReady: true,
        cmd: { push: (callback: () => void) => (callback(), 1) },
        display: vi.fn(),
        getConfig: vi.fn(() => ({ disableInitialLoad: false })),
        pubads: vi.fn(() => pubads),
        setConfig: vi.fn(),
      },
      performance: { now: () => 17 },
    });
    const integrationIds = GPT_DIAGNOSTICS_TEST_IDS;
    const target: Record<string, unknown> = {};
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest: runtimeManifest(releaseId, integrationIds),
        knownIntegrationIds: integrationIds,
        catalog: runtimeCatalog(integrationIds),
        boot: {
          auctionProjection: {
            version: 1,
            auction: {
              version: 1,
              auctionId: 'production-adapter-cycle',
              results: [{ slot: 'production-adapter-slot', outcome: 'no_bid' }],
            },
            slots: [browserSlotPlacement('production-adapter-slot')],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: true } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: concreteAdapter,
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );
    const physicalSlot = Object.freeze({
      clearTargeting: vi.fn(),
      getAdUnitPath: () => '/123/production-adapter-slot',
      getSlotElementId: () => 'production-adapter-slot',
      getTargeting: vi.fn(() => []),
      setTargeting: vi.fn(),
    });
    const emit = (type: string, fields: Readonly<Record<string, unknown>> = {}): void => {
      const event = { slot: physicalSlot, ...fields };
      for (const listener of listeners.get(type) ?? []) listener(event);
    };

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          composition.createDiagnosticsCapabilityProviderRegistrationForTest()
        )
      ).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          createGptDiagnosticsIntegrationRegistration(releaseId)
        )
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      const navigation = composition.runtimeSessionForTest()?.currentNavigation;
      const slots = composition.slotServiceForTest();
      if (!navigation || !slots) throw new Error('Expected active production adapter composition');
      expect(
        slots.adoptGptSlot(navigation.generation, 'production-adapter-slot', {
          definition: {
            adUnitPath: '/123/production-adapter-slot',
            elementId: 'production-adapter-slot',
            sizes: Object.freeze([[300, 250]]),
          },
          ownership: 'trusted_server',
          slot: physicalSlot,
        })
      ).toEqual({ ok: true });
      const request = slots.request({
        intentId: 'production-adapter-request',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'production-adapter-slot',
        requestClass: 'primary',
      });
      await vi.waitFor(() =>
        expect(refresh).toHaveBeenCalledExactlyOnceWith(
          [physicalSlot],
          Object.freeze({ changeCorrelator: false })
        )
      );

      emit('slotRequested');
      emit('slotRenderEnded', { isEmpty: false, responseIdentifier: 'production-response' });
      await expect(request.result).resolves.toEqual({
        responseIdentifier: 'production-response',
        status: 'rendered',
      });
      const diagnostics = target['diagnostics'] as {
        readonly renderTrace: { current(): Readonly<Record<string, unknown>> };
      };
      expect(diagnostics.renderTrace.current()['production-adapter-slot']).toEqual(
        expect.objectContaining({
          gamEmpty: false,
          path: 'gam-refresh',
          rendered: true,
        })
      );
      expect(listeners.get('slotRequested')).toHaveLength(1);
      expect(listeners.get('slotRenderEnded')).toHaveLength(1);
    } finally {
      composition.runtime.dispose();
    }
  });

  it('activates reversible core effects in exact order and disposes them in reverse', async () => {
    const target = {};
    const order: string[] = [];
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId: 'a'.repeat(64),
        manifest: runtimeManifest('a'.repeat(64), ['test']),
        knownIntegrationIds: Object.freeze(['test']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'boot', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: {
          addAdUnits: vi.fn(),
          diagnostics: Object.freeze({}),
          requestAds: vi.fn(),
        },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          prebid: fakePrebidAdapter(),
          messaging: fakeMessagingAdapter(() => {
            order.push('bridge');
            return () => order.push('dispose-bridge');
          }),
        },
        coreActivations: {
          correctnessGptListeners: ({ onDispose }, adapters) => {
            expect(Object.isFrozen(adapters)).toBe(true);
            onDispose(() => order.push('dispose-gpt'));
            order.push('gpt');
          },
        },
      }
    );

    expect(composition.runtime.state).toBe('unclaimed');
    expect(composition.runtime.start()).toBe(true);
    expect(
      composition.runtime.registerIntegration(
        testTakeoverRegistration(
          'test',
          'a'.repeat(64),
          ({
            interfaces,
            onDispose,
          }: {
            interfaces: Readonly<Record<string, unknown>>;
            onDispose(callback: () => void): void;
          }) => {
            expect(interfaces).not.toHaveProperty('diagnostics');
            onDispose(() => order.push('dispose-module'));
            return Object.freeze({ activate: () => order.push('module') });
          }
        )
      )
    ).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(order).toEqual(['bridge', 'gpt', 'module']);
    expect(composition.pucBridgeForTest()).toBeDefined();
    const diagnostics = (
      target as {
        diagnostics?: {
          renderTrace?: {
            current(): Readonly<Record<string, unknown>>;
            history(): readonly unknown[];
            subscribe(listener: (record: unknown) => void): () => void;
          };
        };
      }
    ).diagnostics;
    expect(Object.isFrozen(diagnostics)).toBe(true);
    expect(Reflect.ownKeys(diagnostics ?? {})).toEqual(['renderTrace']);
    expect(Reflect.ownKeys(diagnostics?.renderTrace ?? {}).sort()).toEqual([
      'current',
      'history',
      'subscribe',
    ]);
    expect(diagnostics).not.toHaveProperty('publish');
    expect(diagnostics).not.toHaveProperty('dispose');

    composition.runtime.dispose();
    expect(order).toEqual([
      'bridge',
      'gpt',
      'module',
      'dispose-module',
      'dispose-gpt',
      'dispose-bridge',
    ]);
    expect(composition.pucBridgeForTest()).toBeUndefined();
    expect(() => composition.adapters.googletag.run(() => undefined)).toThrowError(
      expect.objectContaining({ code: 'operation_disposed' })
    );
    expect(() => composition.adapters.prebid.run(() => undefined)).toThrowError(
      expect.objectContaining({ code: 'operation_disposed' })
    );
    expect(Object.isFrozen(composition)).toBe(true);
    expect(Object.isFrozen(composition.runtime)).toBe(true);
    expect(diagnostics?.renderTrace?.current()).toEqual({});
    expect(diagnostics?.renderTrace?.history()).toEqual([]);
  });

  it('starts core slot listeners before module activation and disposes both listeners', async () => {
    const releaseId = 'a'.repeat(64);
    const subscriptions: string[] = [];
    const releases: string[] = [];
    const facade = {
      bindingToken: () => Object.freeze({}),
      subscribe: (eventType: string) => {
        subscriptions.push(eventType);
        return () => releases.push(eventType);
      },
    } as unknown as GoogletagFacade;
    const googletag: GoogletagAdapter = Object.freeze({
      bindingStatus: () => 'present',
      diagnosticsIdentity: () => undefined,
      dispose: vi.fn(),
      notifyReady: vi.fn(),
      observeDiagnostics: () => vi.fn(),
      observePublisherCalls: () => vi.fn(),
      traceToken: () => undefined,
      run: <T>(command: (gpt: Readonly<GoogletagFacade>) => T) => {
        const result = Promise.resolve(command(facade));
        return Object.freeze({ status: 'present' as const, result, dispose: vi.fn() });
      },
    });
    const correctness = vi.fn(
      (
        _context: unknown,
        _adapters: unknown,
        services: { readonly slots: { readonly snapshotForTest: () => { records: number } } }
      ) => {
        expect(subscriptions).toEqual(['slotRequested', 'slotRenderEnded']);
        expect(services.slots.snapshotForTest().records).toBe(0);
      }
    );
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, ['gpt']),
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag,
          messaging: fakeMessagingAdapter(() => {
            expect(subscriptions).toEqual([]);
            return vi.fn();
          }),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: {
          correctnessGptListeners: correctness,
        },
        gptStartupForTest: () => {
          expect(subscriptions).toEqual(['slotRequested', 'slotRenderEnded']);
        },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    expect(
      composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
    ).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(correctness).toHaveBeenCalledOnce();
    composition.runtime.dispose();
    expect(releases).toEqual(['slotRenderEnded', 'slotRequested']);
  });

  it('activates one six-fact GPT diagnostics stream and publishes only diagnostics.gpt', async () => {
    const releaseId = 'a'.repeat(64);
    const target: Record<string, unknown> = {};
    const gpt = synchronousGptAdapter();
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest: runtimeManifest(releaseId, GPT_DIAGNOSTICS_TEST_IDS),
        knownIntegrationIds: GPT_DIAGNOSTICS_TEST_IDS,
        catalog: runtimeCatalog(GPT_DIAGNOSTICS_TEST_IDS),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: true } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: gpt.adapter,
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    expect(
      composition.runtime.registerIntegration(
        composition.createDiagnosticsCapabilityProviderRegistrationForTest()
      )
    ).toBe(true);
    expect(
      composition.runtime.registerIntegration(
        createGptDiagnosticsIntegrationRegistration(releaseId)
      )
    ).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });

    expect(gpt.diagnosticsObserverActive()).toBe(true);
    expect(
      [...gpt.listenerInventory()].sort(([left], [right]) => left.localeCompare(right))
    ).toEqual([
      ['impressionViewable', 1],
      ['slotOnload', 1],
      ['slotRenderEnded', 1],
      ['slotRequested', 1],
      ['slotResponseReceived', 1],
      ['slotVisibilityChanged', 1],
    ]);
    const diagnostics = target['diagnostics'] as
      | {
          readonly gpt?: {
            snapshot(): { readonly slots: readonly { readonly slotElementId?: string }[] };
          };
          readonly renderTrace?: object;
        }
      | undefined;
    expect(Reflect.ownKeys(diagnostics ?? {}).sort()).toEqual(['gpt', 'renderTrace']);
    expect(Reflect.ownKeys(diagnostics?.gpt ?? {}).sort()).toEqual(
      ['export', 'hide', 'show', 'snapshot', 'subscribe'].sort()
    );
    expect(diagnostics).not.toHaveProperty('publish');
    expect(target['gptDiagnostics']).toBeUndefined();
    expect(target['__tsjs_gpt_diagnostics_runtime']).toBeUndefined();

    const observedSlot = Object.freeze({
      getSlotElementId: () => 'composition-slot',
      getAdUnitPath: () => '/example/composition-slot',
    });
    gpt.emit('slotRequested', { slot: observedSlot });
    gpt.emit('slotResponseReceived', { slot: observedSlot });
    expect(diagnostics?.gpt?.snapshot().slots[0]?.slotElementId).toBe('composition-slot');

    composition.runtime.dispose();
    await Promise.resolve();
    expect(gpt.diagnosticsObserverActive()).toBe(false);
    expect(gpt.listenerInventory()).toEqual([]);
  });

  it('keeps the core diagnostics ingress private from integration modules', async () => {
    const releaseId = 'a'.repeat(64);
    const gpt = synchronousGptAdapter();
    const integrationIds = Object.freeze([
      BROWSER_TEST_DIAGNOSTICS_PROVIDER_ID,
      'diagnostics_probe',
      'gpt_diagnostics',
      'diagnostics_presentation',
    ]);
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, integrationIds),
        knownIntegrationIds: integrationIds,
        catalog: runtimeCatalog(integrationIds),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: true } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: gpt.adapter,
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          composition.createDiagnosticsCapabilityProviderRegistrationForTest()
        )
      ).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          testTakeoverRegistration(
            'diagnostics_probe',
            releaseId,
            ({ interfaces }: IntegrationPrepareContext) => {
              expect(interfaces).not.toHaveProperty('diagnostics');
              const trace = interfaces['trace.v1'] as Readonly<Record<string, unknown>>;
              expect(Reflect.ownKeys(trace).sort()).toEqual(
                ['diagnostics', 'enrich', 'observations', 'prune', 'record'].sort()
              );
              expect(Reflect.ownKeys(trace['observations'] as object)).toEqual(['publish']);
              expect(trace).not.toHaveProperty('attachPresentation');
              return Object.freeze({ activate: vi.fn() });
            }
          )
        )
      ).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          createGptDiagnosticsIntegrationRegistration(releaseId)
        )
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    } finally {
      composition.runtime.dispose();
    }
  });

  it('routes safe GPT facts into the same-impression render trace state machine', async () => {
    const releaseId = 'a'.repeat(64);
    const target: Record<string, unknown> = {};
    const gpt = synchronousGptAdapter();
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest: runtimeManifest(releaseId, GPT_DIAGNOSTICS_TEST_IDS),
        knownIntegrationIds: GPT_DIAGNOSTICS_TEST_IDS,
        catalog: runtimeCatalog(GPT_DIAGNOSTICS_TEST_IDS),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: true } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: gpt.adapter,
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          composition.createDiagnosticsCapabilityProviderRegistrationForTest()
        )
      ).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          createGptDiagnosticsIntegrationRegistration(releaseId)
        )
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      const addAdUnits = target['addAdUnits'] as (unit: unknown) => unknown;
      addAdUnits({
        code: 'gpt-trace-slot',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      });
      const physicalSlot = Object.freeze({
        getSlotElementId: () => 'gpt-trace-slot',
        getAdUnitPath: () => '/example/gpt-trace-slot',
      });
      const navigation = composition.runtimeSessionForTest()?.currentNavigation;
      if (!navigation) throw new Error('Expected active navigation');
      expect(
        composition.slotServiceForTest()?.adoptGptSlot(navigation.generation, 'gpt-trace-slot', {
          ownership: 'publisher',
          slot: physicalSlot,
        })
      ).toEqual({ ok: true });
      expect(gpt.listenerRoles('slotRequested')).toEqual([false]);

      gpt.emit('slotRequested', { slot: physicalSlot });
      expect(composition.slotServiceForTest()?.snapshotForTest().cycles).toBe(1);
      gpt.emit('slotRenderEnded', { slot: physicalSlot, isEmpty: false });
      gpt.emit('impressionViewable', { slot: physicalSlot });
      expect(gpt.diagnosticFacts().map((fact) => [fact.kind, fact.slot.cycleOrdinal])).toEqual([
        ['slotRequested', 1],
        ['slotRenderEnded', 1],
        ['impressionViewable', 1],
      ]);

      const diagnostics = target['diagnostics'] as {
        renderTrace: {
          current(): Readonly<Record<string, Readonly<Record<string, unknown>>>>;
          history(): readonly Readonly<Record<string, unknown>>[];
        };
      };
      expect(diagnostics.renderTrace.current()['gpt-trace-slot']).toEqual(
        expect.objectContaining({
          path: 'gam-refresh',
          rendered: true,
          gamEmpty: false,
          injected: false,
          visible: true,
          servedFrom: 'gam',
        })
      );
      expect(diagnostics.renderTrace.history()).toHaveLength(1);
    } finally {
      composition.runtime.dispose();
    }
  });

  it('reconciles a trusted terminal that arrives after the GPT render fact', async () => {
    const releaseId = 'a'.repeat(64);
    const target: Record<string, unknown> = {};
    const gpt = synchronousGptAdapter();
    const renderSource = Object.freeze({
      type: 'adm' as const,
      version: 1 as const,
      adm: '<div>reverse-order winner</div>',
      width: 300,
      height: 250,
    });
    const auctionFetcher = vi.fn(async () => ({
      ok: true,
      json: async () => ({
        id: 'reverse-auction',
        cur: 'USD',
        seatbid: [
          {
            seat: 'fictional',
            bid: [
              {
                id: 'r1_AAAAAAAAAAAAAAAAAAAAAA',
                impid: 'reverse-order-slot',
                price: 1,
                adm: renderSource.adm,
                w: renderSource.width,
                h: renderSource.height,
                ext: {
                  trusted_server: {
                    candidate_id: 'AAAAAAAAAAAA',
                    slot_id: 'reverse-order-slot',
                    render_source: renderSource,
                  },
                },
              },
            ],
          },
        ],
        ext: {
          trusted_server: {
            slot_results: {
              version: 1,
              auctionId: 'reverse-auction',
              results: [
                {
                  slot: 'reverse-order-slot',
                  outcome: 'winner',
                  candidateId: 'AAAAAAAAAAAA',
                },
              ],
            },
          },
        },
      }),
    }));
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest: runtimeManifest(releaseId, GPT_DIAGNOSTICS_TEST_IDS),
        knownIntegrationIds: GPT_DIAGNOSTICS_TEST_IDS,
        catalog: runtimeCatalog(GPT_DIAGNOSTICS_TEST_IDS),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: true } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: gpt.adapter,
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        auctionFetcherForTest: auctionFetcher,
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          composition.createDiagnosticsCapabilityProviderRegistrationForTest()
        )
      ).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          createGptDiagnosticsIntegrationRegistration(releaseId)
        )
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      const api = target as {
        addAdUnits(value: unknown): unknown;
        requestAds(options: unknown): Promise<unknown>;
        diagnostics: {
          renderTrace: {
            current(): Readonly<Record<string, Readonly<Record<string, unknown>>>>;
            history(): readonly Readonly<Record<string, unknown>>[];
          };
        };
      };
      api.addAdUnits({
        code: 'reverse-order-slot',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      });
      document.body.innerHTML = '<div id="reverse-order-slot"></div>';
      const physicalSlot = Object.freeze({
        getSlotElementId: () => 'reverse-order-slot',
        getAdUnitPath: () => '/example/reverse-order-slot',
      });
      const navigation = composition.runtimeSessionForTest()?.currentNavigation;
      if (!navigation) throw new Error('Expected active navigation');
      expect(
        composition
          .slotServiceForTest()
          ?.adoptGptSlot(navigation.generation, 'reverse-order-slot', {
            ownership: 'publisher',
            slot: physicalSlot,
          })
      ).toEqual({ ok: true });
      gpt.emit('slotRequested', { slot: physicalSlot });
      gpt.emit('slotRenderEnded', { slot: physicalSlot, isEmpty: false });
      const provisional = api.diagnostics.renderTrace.current()['reverse-order-slot'];

      const request = api.requestAds({ slots: ['reverse-order-slot'] });
      await vi.waitFor(() =>
        expect(document.querySelector('#reverse-order-slot iframe')).not.toBeNull()
      );
      document
        .querySelector<HTMLIFrameElement>('#reverse-order-slot iframe')
        ?.dispatchEvent(new Event('load'));
      await expect(request).resolves.toEqual({
        slots: [{ slot: 'reverse-order-slot', path: 'primary', outcome: 'accepted' }],
      });

      expect(api.diagnostics.renderTrace.current()['reverse-order-slot']).toEqual(
        expect.objectContaining({
          seq: provisional?.['seq'],
          count: provisional?.['count'],
          at: provisional?.['at'],
          path: 'auction',
          rendered: true,
          injected: true,
          gamEmpty: false,
          servedFrom: 'inline',
        })
      );
      expect(api.diagnostics.renderTrace.history()).toHaveLength(1);
      gpt.emit('slotVisibilityChanged', { slot: physicalSlot, inViewPercentage: 0 });
      expect(api.diagnostics.renderTrace.current()['reverse-order-slot']).toEqual(
        expect.objectContaining({ seq: provisional?.['seq'], path: 'auction', visible: false })
      );
      expect(api.diagnostics.renderTrace.history()).toHaveLength(1);
    } finally {
      composition.runtime.dispose();
      document.body.innerHTML = '';
    }
  });

  it('injects GPT and Prebid module boundaries with only server-frozen configuration', async () => {
    const releaseId = 'a'.repeat(64);
    const target = {};
    const gptConfig = Object.freeze({ gamAttributionEnabled: false });
    const prebidConfig = Object.freeze({
      accountId: 'test',
      timeout: 1_000,
      debug: false,
      bidders: Object.freeze(['rubicon']),
      clientSideBidders: Object.freeze(['rubicon']),
      excludedGamAdUnitPathSuffixes: Object.freeze<string[]>([]),
    });
    const providedBindings = vi.fn((id: string) => ({
      config: id === 'prebid' ? prebidConfig : gptConfig,
      interfaces: Object.freeze({ publisherControlled: Object.freeze({}) }),
    }));
    const startGpt = vi.fn((received: unknown) => {
      expect(received).toEqual(gptConfig);
      expect(Object.isFrozen(received)).toBe(true);
      expect((target as { version?: unknown }).version).toBe('1.0.0');
    });
    const startPrebid = vi.fn((received: unknown) => {
      expect(received).toEqual(prebidConfig);
      expect(Object.isFrozen(received)).toBe(true);
      expect((target as { version?: unknown }).version).toBe('1.0.0');
    });
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest: runtimeManifest(releaseId, ['gpt', 'prebid']),
        knownIntegrationIds: Object.freeze(['gpt', 'prebid']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          integrations: {
            version: 1,
            entries: [
              { id: 'gpt', config: gptConfig },
              { id: 'prebid', config: prebidConfig },
            ],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: providedBindings,
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(() => vi.fn()),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        gptStartupForTest: startGpt,
        prebidStartupForTest: startPrebid,
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
      ).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          createLegacyPrebidIntegrationRegistration(releaseId)
        )
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });

      expect(providedBindings).toHaveBeenCalledTimes(2);
      expect(providedBindings).toHaveBeenNthCalledWith(1, 'gpt');
      expect(providedBindings).toHaveBeenNthCalledWith(2, 'prebid');
      expect(startGpt).toHaveBeenCalledExactlyOnceWith(gptConfig);
      expect(startPrebid).toHaveBeenCalledExactlyOnceWith(prebidConfig);
      expect(isGuardInstalled()).toBe(true);
      expect(composition.runtimeSessionForTest()?.interfaces).not.toHaveProperty(
        'publisherControlled'
      );
    } finally {
      composition.runtime.dispose();
      resetGuardState();
    }
    expect(isGuardInstalled()).toBe(false);
  });

  it('composes the configured Prebid refresh policy through the owned GPT boundary', async () => {
    const releaseId = 'a'.repeat(64);
    const target: Record<string, unknown> = {};
    const gpt = synchronousGptAdapter();
    const prebid = synchronousPrebidAdapter();
    const prebidConfig = Object.freeze({
      accountId: 'test',
      timeout: 1_500,
      debug: false,
      bidders: Object.freeze(['client']),
      clientSideBidders: Object.freeze(['client']),
      excludedGamAdUnitPathSuffixes: Object.freeze<string[]>([]),
    });
    let request:
      | Readonly<{
          adUnits: readonly object[];
          bidsBackHandler: () => void;
          timeout: number;
        }>
      | undefined;
    prebid.requestBids.mockImplementation((candidate: unknown) => {
      request = candidate as typeof request;
      request?.bidsBackHandler();
    });
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest: runtimeManifest(releaseId, ['gpt', 'prebid']),
        knownIntegrationIds: Object.freeze(['gpt', 'prebid']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          integrations: {
            version: 1,
            entries: [
              { id: 'gpt', config: { gamAttributionEnabled: false } },
              { id: 'prebid', config: prebidConfig },
            ],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: (id) => ({
          config: id === 'prebid' ? prebidConfig : Object.freeze({}),
          interfaces: Object.freeze({}),
        }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: gpt.adapter,
          messaging: fakeMessagingAdapter(),
          prebid: prebid.adapter,
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    expect(
      composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
    ).toBe(true);
    expect(
      composition.runtime.registerIntegration(createLegacyPrebidIntegrationRegistration(releaseId))
    ).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });

    const api = target as {
      addAdUnits(unit: unknown): Readonly<{ registered: readonly string[] }>;
    };
    expect(
      api.addAdUnits({
        code: 'refresh-slot',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [
          { bidder: 'server', params: { placement: 7 } },
          { bidder: 'client', params: { placement: 'browser' } },
        ],
      })
    ).toEqual({ registered: ['refresh-slot'] });
    const navigation = composition.runtimeSessionForTest()?.currentNavigation;
    const slots = composition.slotServiceForTest();
    const physicalSlot = Object.freeze({ getAdUnitPath: () => '/network/refresh-slot' });
    if (!navigation || !slots) throw new Error('Expected the active refresh composition');
    expect(
      slots.adoptGptSlot(navigation.generation, 'refresh-slot', {
        definition: {
          adUnitPath: '/network/refresh-slot',
          elementId: 'refresh-slot',
          sizes: Object.freeze([[300, 250]]),
        },
        ownership: 'publisher',
        slot: physicalSlot,
      })
    ).toEqual({ ok: true });

    const refreshOptions = Object.freeze({ changeCorrelator: false });
    const decision = gpt.publisherRefresh(
      Object.freeze({
        requestedSlots: Object.freeze([physicalSlot]),
        slots: Object.freeze([physicalSlot]),
        options: refreshOptions,
      })
    );
    expect(decision).toMatchObject({
      action: 'defer',
      slots: [physicalSlot],
      completion: expect.any(Promise),
    });
    if (decision?.action !== 'defer') throw new Error('Expected the composed refresh policy');
    await decision.completion;

    expect(request?.timeout).toBe(1_500);
    expect(request?.adUnits).toEqual([
      {
        code: 'refresh-slot',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
        bids: [
          {
            bidder: 'trustedServer',
            params: { bidderParams: { server: { placement: 7 } } },
          },
          { bidder: 'client', params: { placement: 'browser' } },
        ],
      },
    ]);
    expect(prebid.setTargetingForGpt).toHaveBeenCalledExactlyOnceWith(['refresh-slot']);
    composition.runtime.dispose();
  });

  it('owns every remaining integration in one maximal composed transaction', async () => {
    vi.useFakeTimers();
    const releaseId = 'a'.repeat(64);
    const target = {};
    const takeoverMembers = Object.freeze([
      ['render_runtime', createRenderRuntimeIntegrationRegistration] as const,
      ['datadome', createDataDomeIntegrationRegistration] as const,
      ['didomi', createDidomiIntegrationRegistration] as const,
      ['google_tag_manager', createGoogleTagManagerIntegrationRegistration] as const,
      ['lockr', createLockrIntegrationRegistration] as const,
      ['osano_consent', createOsanoIntegrationRegistration] as const,
      ['permutive_context', createPermutiveIntegrationRegistration] as const,
      ['sourcepoint_consent', createSourcepointIntegrationRegistration] as const,
      ['testlight', createTestlightIntegrationRegistration] as const,
    ]);
    const deferredMembers = Object.freeze([
      ['osano_lifecycle', createOsanoLifecycleIntegrationRegistration] as const,
      ['permutive_lifecycle', createPermutiveLifecycleIntegrationRegistration] as const,
      ['sourcepoint_lifecycle', createSourcepointLifecycleIntegrationRegistration] as const,
    ]);
    const members = Object.freeze([...takeoverMembers, ...deferredMembers]);
    const ids = Object.freeze(members.map(([id]) => id));
    const configFor = (id: string): unknown => {
      if (id === 'didomi') return Object.freeze({ proxyPath: '/integrations/didomi/consent/' });
      if (id === 'sourcepoint_consent') return Object.freeze({ rewriteSdk: true });
      return undefined;
    };
    const manifest = runtimeManifest(releaseId, ids);
    expect(manifest.integrations.map(({ id, phase }) => [id, phase])).toEqual([
      ['render_runtime', 'takeover'],
      ['datadome', 'takeover'],
      ['didomi', 'takeover'],
      ['google_tag_manager', 'takeover'],
      ['lockr', 'takeover'],
      ['osano_consent', 'takeover'],
      ['permutive_context', 'takeover'],
      ['sourcepoint_consent', 'takeover'],
      ['testlight', 'takeover'],
      ['osano_lifecycle', 'deferred'],
      ['permutive_lifecycle', 'deferred'],
      ['sourcepoint_lifecycle', 'deferred'],
    ]);
    const runtimeScript = document.createElement('script');
    runtimeScript.id = 'trustedserver-js';
    runtimeScript.src = new URL(manifest.runtimeSrc, window.location.origin).href;
    document.head.append(runtimeScript);
    let executingScript: HTMLScriptElement | null = runtimeScript;
    const currentScript = vi
      .spyOn(document, 'currentScript', 'get')
      .mockImplementation(() => executingScript);
    const appendChildBefore = Element.prototype.appendChild;
    const insertBeforeBefore = Element.prototype.insertBefore;
    const didomiBefore = Object.getOwnPropertyDescriptor(window, 'didomiConfig');
    const testlightBefore = Object.getOwnPropertyDescriptor(window, 'testlight');
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest,
        knownIntegrationIds: ids,
        catalog: runtimeCatalog(ids),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: (id) => ({ config: configFor(id), interfaces: Object.freeze({}) }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );
    const originalHeadAppend = document.head.append.bind(document.head);
    const loadedDeferredIds: string[] = [];
    const headAppend = vi.spyOn(document.head, 'append').mockImplementation((...nodes) => {
      originalHeadAppend(...nodes);
      for (const node of nodes) {
        if (!(node instanceof HTMLScriptElement) || node === runtimeScript) continue;
        const member = deferredMembers.find(([id]) => node.src.includes(`tsjs-${id}.min.js`));
        if (!member) continue;
        executingScript = node;
        loadedDeferredIds.push(member[0]);
        expect(composition.runtime.registerIntegration(member[1](releaseId))).toBe(true);
        node.onload?.(new Event('load'));
        executingScript = runtimeScript;
      }
    });

    expect(composition.runtime.start()).toBe(true);
    for (const [, createRegistration] of takeoverMembers) {
      expect(composition.runtime.registerIntegration(createRegistration(releaseId))).toBe(true);
    }
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(composition.runtime.protectFirstDisplayAttemptBatch([Promise.resolve()])).toBe(true);
    await vi.advanceTimersByTimeAsync(2_500);
    expect(loadedDeferredIds).toEqual(deferredMembers.map(([id]) => id));
    expect(composition.auctionContextRegistryForTest()?.snapshotInventoryForTest()).toEqual({
      disposed: false,
      registrations: [],
    });
    expect(vi.getTimerCount()).toBeGreaterThan(0);

    composition.runtime.dispose();
    composition.runtime.dispose();
    expect(vi.getTimerCount()).toBe(0);
    expect(Element.prototype.appendChild).toBe(appendChildBefore);
    expect(Element.prototype.insertBefore).toBe(insertBeforeBefore);
    expect(Object.getOwnPropertyDescriptor(window, 'didomiConfig')).toEqual(didomiBefore);
    expect(Object.getOwnPropertyDescriptor(window, 'testlight')).toEqual(testlightBefore);
    headAppend.mockRestore();
    currentScript.mockRestore();
    runtimeScript.remove();
  });

  it('injects the exact creative boot into reversible activation and post-commit startup', async () => {
    const releaseId = 'a'.repeat(64);
    const creative = Object.freeze({
      version: 1 as const,
      enabled: true,
      clickGuard: true,
      renderGuard: false,
    });
    const release = vi.fn();
    const activateCreative = vi.fn((received: unknown) => {
      expect(received).toEqual(creative);
      expect(Object.isFrozen(received)).toBe(true);
      return release;
    });
    const startCreative = vi.fn((received: unknown) => {
      expect(received).toEqual(creative);
      expect(Object.isFrozen(received)).toBe(true);
    });
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, ['creative']),
        knownIntegrationIds: Object.freeze(['creative']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative,
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        creativeActivationForTest: activateCreative,
        creativeStartupForTest: startCreative,
      }
    );

    expect(composition.runtime.start()).toBe(true);
    expect(
      composition.runtime.registerIntegration(
        createLegacyCreativeIntegrationRegistration(releaseId)
      )
    ).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(activateCreative).toHaveBeenCalledTimes(1);
    expect(startCreative).toHaveBeenCalledTimes(1);
    expect(activateCreative.mock.calls[0]?.[0]).toBe(startCreative.mock.calls[0]?.[0]);

    composition.runtime.dispose();
    composition.runtime.dispose();
    expect(release).toHaveBeenCalledTimes(1);
  });

  it('commits enabled creative with both guards false without creative effects', async () => {
    const releaseId = 'a'.repeat(64);
    const activateCreative = vi.fn();
    const startCreative = vi.fn();
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, []),
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: true, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        creativeActivationForTest: activateCreative,
        creativeStartupForTest: startCreative,
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(activateCreative).not.toHaveBeenCalled();
    expect(startCreative).not.toHaveBeenCalled();

    composition.runtime.dispose();
  });

  it.each([
    [
      'disabled click guard bit',
      { version: 1, enabled: false, clickGuard: true, renderGuard: false },
      [],
    ],
    [
      'disabled render guard bit',
      { version: 1, enabled: false, clickGuard: false, renderGuard: true },
      [],
    ],
    [
      'disabled creative manifest member',
      { version: 1, enabled: false, clickGuard: false, renderGuard: false },
      ['creative'],
    ],
    [
      'missing enabled creative manifest member',
      { version: 1, enabled: true, clickGuard: true, renderGuard: false },
      [],
    ],
  ] as const)('rejects creative ABI mismatch: %s', async (_caseName, creative, manifestIds) => {
    const releaseId = 'a'.repeat(64);
    const activateCreative = vi.fn();
    const startCreative = vi.fn();
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, manifestIds),
        knownIntegrationIds: Object.freeze(['creative']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative,
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        creativeActivationForTest: activateCreative,
        creativeStartupForTest: startCreative,
      }
    );

    expect(composition.runtime.start()).toBe(true);
    if (manifestIds.length === 1) {
      expect(
        composition.runtime.registerIntegration(
          createProductionCreativeIntegrationRegistration(releaseId)
        )
      ).toBe(true);
    }
    await expect(composition.runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'abi_mismatch',
    });
    expect(activateCreative).not.toHaveBeenCalled();
    expect(startCreative).not.toHaveBeenCalled();
  });

  it('owns the real creative click guard through the composition lifecycle', async () => {
    const releaseId = 'a'.repeat(64);
    const addEventListener = vi.spyOn(document, 'addEventListener');
    const removeEventListener = vi.spyOn(document, 'removeEventListener');
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, ['creative']),
        knownIntegrationIds: Object.freeze(['creative']),
        catalog: runtimeCatalog(Object.freeze(['creative'])),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: true, clickGuard: true, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          createProductionCreativeIntegrationRegistration(releaseId)
        )
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      expect(addEventListener.mock.calls.filter(([type]) => type === 'click')).toHaveLength(1);
      expect(addEventListener.mock.calls.filter(([type]) => type === 'auxclick')).toHaveLength(1);
    } finally {
      composition.runtime.dispose();
      addEventListener.mockRestore();
    }
    expect(removeEventListener.mock.calls.filter(([type]) => type === 'click')).toHaveLength(1);
    expect(removeEventListener.mock.calls.filter(([type]) => type === 'auxclick')).toHaveLength(1);
    removeEventListener.mockRestore();
  });

  it('publishes and promotes one exact Prebid winner through runtime-owned PUC state', async () => {
    const releaseId = 'a'.repeat(64);
    const prebid = synchronousPrebidAdapter();
    const reservationId = `r1_${'p'.repeat(22)}`;
    let captureListener: CaptureMessageListener | undefined;
    const messagingTarget = {
      addEventListener: vi.fn(
        (_type: 'message', listener: CaptureMessageListener, _capture: true) => {
          captureListener = listener;
        }
      ),
      removeEventListener: vi.fn(),
    };
    const messaging = createBrowserMessagingAdapter(messagingTarget);
    const bid = Object.freeze({
      candidateId: 'AAAAAAAAAAAA',
      slot: 'slot-one',
      provider: 'trusted',
      upstreamBidId: 'upstream-one',
      cpm: 1.25,
      currency: 'USD' as const,
      targeting: Object.freeze({ hb_bidder: 'trustedServer' }),
      rendererReservationId: reservationId,
      renderSource: Object.freeze({
        type: 'adm' as const,
        version: 1 as const,
        adm: '<main>private creative</main>',
        width: 300,
        height: 250,
      }),
    });
    const projection = Object.freeze({
      version: 1,
      auction: Object.freeze({
        version: 1,
        auctionId: 'auction-one',
        results: Object.freeze([
          Object.freeze({
            slot: bid.slot,
            outcome: 'winner' as const,
            candidateId: bid.candidateId,
          }),
        ]),
      }),
      slots: Object.freeze([browserSlotPlacement(bid.slot)]),
      bids: Object.freeze([bid]),
    });
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, ['prebid']),
        knownIntegrationIds: Object.freeze(['prebid']),
        boot: {
          auctionProjection: projection,
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging,
          prebid: prebid.adapter,
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        createIdentityIssuerForTest: () =>
          createTestNavigationIdentityIssuer({
            getRandomValues: (target) => {
              target.fill(9);
              return target;
            },
          }),
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          createLegacyPrebidIntegrationRegistration(releaseId)
        )
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      const complete = vi.fn();
      prebid.auction(
        Object.freeze({
          auctionId: 'auction-one',
          bids: Object.freeze([Object.freeze({ adUnitCode: bid.slot, requestId: 'request-one' })]),
          complete,
        })
      );

      expect(complete).toHaveBeenCalledTimes(1);
      expect(prebid.admitTrustedBid).toHaveBeenCalledTimes(1);
      expect(prebid.admitTrustedBid.mock.calls[0]?.[0]).toMatchObject({
        auctionId: 'auction-one',
        adUnitCode: bid.slot,
        bid: { adId: reservationId, requestId: 'request-one' },
      });
      expect(composition.reservationServiceForTest()?.recognize(reservationId)).toMatchObject({
        state: 'awaiting_prebid_selection',
      });

      prebid.auctionEnd('auction-one');
      expect(composition.reservationServiceForTest()?.recognize(reservationId)).toMatchObject({
        state: 'renderable',
      });
      expect(composition.pucBridgeForTest()?.snapshotInventoryForTest()).toMatchObject({
        attempts: 1,
      });

      const claimPort = {
        addEventListener: vi.fn(),
        close: vi.fn(),
        postMessage: vi.fn(),
        removeEventListener: vi.fn(),
        start: vi.fn(),
      };
      captureListener?.({
        data: JSON.stringify({
          message: 'Prebid Request',
          adId: reservationId,
          adServerDomain: 'ads.example.com',
        }),
        ports: [claimPort],
        source: Object.freeze({ frame: 'selected-creative' }),
        stopImmediatePropagation: vi.fn(),
      } as unknown as MessageEvent);
      expect(composition.pucBridgeForTest()?.snapshotInventoryForTest()).toMatchObject({
        attempts: 1,
        liveTickets: 0,
        pendingClaims: 1,
      });
      expect(claimPort.postMessage).not.toHaveBeenCalled();
      expect(claimPort.close).not.toHaveBeenCalled();
    } finally {
      composition.runtime.dispose();
    }
  });

  const prebidPublicationFailureCases: readonly (readonly [
    string,
    (prepared: Readonly<PreparedTrustedBidV1>) => 'admitted' | 'not_admitted',
    'prebid_admission_failed' | 'prebid_contract_violation',
  ])[] = [
    ['not admitted', () => 'not_admitted', 'prebid_admission_failed'],
    [
      'partial publication',
      () => {
        throw new PrebidAdmissionContractError();
      },
      'prebid_contract_violation',
    ],
  ];
  it.each(prebidPublicationFailureCases)(
    'settles a %s Prebid publication as an exact slot lifecycle failure',
    async (_case, admission, reason) => {
      const releaseId = 'a'.repeat(64);
      const prebid = synchronousPrebidAdapter(admission);
      const reservationId = `r1_${'q'.repeat(22)}`;
      const bid = Object.freeze({
        candidateId: 'BBBBBBBBBBBB',
        slot: 'failed-slot',
        provider: 'trusted',
        upstreamBidId: 'failed-upstream',
        cpm: 2.5,
        currency: 'USD' as const,
        targeting: Object.freeze({ hb_bidder: 'trustedServer' }),
        rendererReservationId: reservationId,
        renderSource: Object.freeze({
          type: 'adm' as const,
          version: 1 as const,
          adm: '<main>must not render</main>',
          width: 300,
          height: 250,
        }),
      });
      const projection = Object.freeze({
        version: 1,
        auction: Object.freeze({
          version: 1,
          auctionId: 'failed-auction',
          results: Object.freeze([
            Object.freeze({
              slot: bid.slot,
              outcome: 'winner' as const,
              candidateId: bid.candidateId,
            }),
          ]),
        }),
        slots: Object.freeze([browserSlotPlacement(bid.slot)]),
        bids: Object.freeze([bid]),
      });
      const composition = createTestBrowserRuntimeComposition(
        {
          target: {},
          releaseId,
          manifest: runtimeManifest(releaseId, ['prebid', 'lifecycle_probe']),
          knownIntegrationIds: Object.freeze(['prebid', 'lifecycle_probe']),
          boot: {
            auctionProjection: projection,
            creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
            diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
          },
          getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
          kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
        },
        {
          adapters: {
            googletag: fakeGoogletagAdapter(),
            messaging: fakeMessagingAdapter(),
            prebid: prebid.adapter,
          },
          coreActivations: { correctnessGptListeners: vi.fn() },
        }
      );

      try {
        expect(composition.runtime.start()).toBe(true);
        expect(
          composition.runtime.registerIntegration(
            createLegacyPrebidIntegrationRegistration(releaseId)
          )
        ).toBe(true);
        expect(
          composition.runtime.registerIntegration(
            testTakeoverRegistration(
              'lifecycle_probe',
              releaseId,
              ({ interfaces }: { interfaces: Readonly<Record<string, unknown>> }) => {
                expect(interfaces).not.toHaveProperty('diagnostics');
                return Object.freeze({ activate: vi.fn() });
              }
            )
          )
        ).toBe(true);
        await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
        const complete = vi.fn();

        prebid.auction(
          Object.freeze({
            auctionId: 'failed-auction',
            bids: Object.freeze([
              Object.freeze({ adUnitCode: bid.slot, requestId: 'failed-request' }),
            ]),
            complete,
          })
        );

        expect(complete).toHaveBeenCalledOnce();
        expect(composition.reservationServiceForTest()?.recognize(reservationId)).toMatchObject({
          recognized: true,
          state: reason,
        });
        expect(
          composition.runtimeSessionForTest()?.currentNavigation?.snapshotInventoryForTest()
        ).toMatchObject({
          attempts: 0,
          batches: 0,
        });
      } finally {
        composition.runtime.dispose();
      }
    }
  );

  it('hands late publisher GPT calls through the adapter into runtime-owned slot state', async () => {
    const releaseId = 'a'.repeat(64);
    const slot = Object.freeze({ id: 'trusted-slot' });
    const unrelated = Object.freeze({ id: 'publisher-slot' });
    const refresh = vi.fn((_slots?: readonly object[], _options?: unknown) => undefined);
    const display = vi.fn((_target: unknown) => undefined);
    const destroySlots = vi.fn((_slots?: readonly object[]) => true);
    const listeners = new Map<string, Set<(event: unknown) => void>>();
    const pubads = {
      addEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
        const registered = listeners.get(type) ?? new Set();
        registered.add(listener);
        listeners.set(type, registered);
      }),
      disableInitialLoad: vi.fn(),
      getSlots: vi.fn(() => [slot, unrelated]),
      refresh,
      removeEventListener: vi.fn((type: string, listener: (event: unknown) => void) => {
        listeners.get(type)?.delete(listener);
      }),
    };
    const nativeDefineSlot = vi.fn((_path: string, _sizes: unknown, _elementId: string) =>
      Object.freeze({ id: 'duplicate' })
    );
    const googletag = {
      apiReady: true,
      pubadsReady: true,
      cmd: { push: (command: () => void) => (command(), 0) },
      defineSlot: nativeDefineSlot,
      destroySlots,
      display,
      getConfig: vi.fn(() => ({ disableInitialLoad: true })),
      pubads: () => pubads,
      setConfig: vi.fn(),
    };
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, ['gpt']),
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: {
              version: 1,
              auctionId: 'initial',
              results: [{ slot: 'slot', outcome: 'no_bid' }],
            },
            slots: [browserSlotPlacement('slot')],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: createBrowserGoogletagAdapter({ googletag }),
          messaging: fakeMessagingAdapter(() => vi.fn()),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      const navigation = composition.runtimeSessionForTest()?.currentNavigation;
      const slots = composition.slotServiceForTest();
      if (!navigation || !slots) throw new Error('Expected active GPT composition');
      expect(
        slots.adoptGptSlot(navigation.generation, 'slot', {
          definition: {
            adUnitPath: '/trusted/path',
            elementId: 'slot-div',
            sizes: Object.freeze([[300, 250]]),
          },
          elementIdPrefix: 'slot-',
          ownership: 'trusted_server',
          slot,
        })
      ).toEqual({ ok: true });

      expect(googletag.defineSlot('/publisher/mismatch', [728, 90], 'slot-div')).toBe(slot);
      expect(nativeDefineSlot).not.toHaveBeenCalled();
      expect(googletag.display('slot-div')).toBeUndefined();
      expect(display).not.toHaveBeenCalled();
      const options = Object.freeze({ changeCorrelator: true, publisher: 'preserved' });
      expect(pubads.refresh(undefined, options)).toBeUndefined();
      expect(refresh).toHaveBeenCalledExactlyOnceWith([unrelated], options);
      pubads.refresh([slot], options);
      expect(refresh).toHaveBeenLastCalledWith([slot], options);
      const request = slots.request({
        intentId: 'publisher-owned',
        navigationGeneration: navigation.generation,
        operation: 'refresh',
        registeredSlotId: 'slot',
        requestClass: 'primary',
      });
      await expect(request.result).resolves.toEqual({
        status: 'failed',
        reason: 'cycle_unattributable',
      });
      expect(googletag.destroySlots([slot])).toBe(true);
      expect(slots.isBoundGptSlot(navigation.generation, 'slot', slot)).toBe(false);
    } finally {
      composition.runtime.dispose();
      resetGuardState();
    }
    expect(destroySlots).toHaveBeenCalledTimes(1);
  });

  it('constructs one session lazily from accepted boot and keeps it across SPA replacement', async () => {
    const projection = {
      version: 1,
      auction: {
        version: 1,
        auctionId: 'initial',
        results: [{ slot: 'initial-slot', outcome: 'no_bid' }],
      },
      slots: [browserSlotPlacement('initial-slot')],
      bids: [],
    };
    let prefix = 0;
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: runtimeManifest('a'.repeat(64), []),
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: projection,
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: {
          addAdUnits: vi.fn(),
          diagnostics: Object.freeze({}),
          requestAds: vi.fn(),
        },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          prebid: fakePrebidAdapter(),
          messaging: fakeMessagingAdapter(),
        },
        coreActivations: {
          correctnessGptListeners: vi.fn(),
        },
        createIdentityIssuerForTest: () => {
          prefix += 1;
          return createTestNavigationIdentityIssuer({
            getRandomValues: (target) => {
              target.fill(prefix);
              return target;
            },
          });
        },
      }
    );

    expect(composition.runtimeSessionForTest()).toBeUndefined();
    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    const session = composition.runtimeSessionForTest();
    expect(session).toBeDefined();
    expect(composition.runtimeSessionForTest()).toBe(session);
    const slotService = composition.slotServiceForTest();
    const targetingService = composition.targetingServiceForTest();
    const reservationService = composition.reservationServiceForTest();
    const rendererNonces = composition.rendererNonceRegistryForTest();
    expect(slotService).toBeDefined();
    expect(targetingService).toBeDefined();
    expect(reservationService).toBeDefined();
    expect(rendererNonces).toBeDefined();
    expect(session?.interfaces['slots']).toBe(slotService);
    expect(session?.interfaces['targeting']).toBe(targetingService);
    expect(session?.interfaces['reservations']).toBe(reservationService);
    expect(session?.interfaces['rendererNonces']).toBe(rendererNonces);
    expect(session?.interfaces['renderDirectAps']).toBeTypeOf('function');
    expect(session?.interfaces['renderDirectAdm']).toBeTypeOf('function');
    expect(session?.interfaces['renderDirectCache']).toBeUndefined();
    expect(session?.currentNavigation?.interfaces).toBe(session?.interfaces);
    expect(session?.currentNavigation?.currentAuctionProjection).toEqual(projection);
    expect(Object.isFrozen(session?.currentNavigation?.currentAuctionProjection)).toBe(true);

    const initialNavigation = session?.currentNavigation;
    const artifactBatch = initialNavigation?.createAuctionBatch('accepted-artifact');
    const artifactOwner = artifactBatch?.createRenderAttempt('accepted-artifact-slot');
    const artifactStore = session?.interfaces['artifacts'] as
      Parameters<typeof createRenderAttempt>[0]['artifacts'] | undefined;
    if (!artifactOwner?.ok || !artifactStore || !reservationService) {
      throw new Error('Expected accepted-artifact dependencies');
    }
    const acceptedSource = Object.freeze({
      type: 'adm' as const,
      version: 1 as const,
      adm: '<main>accepted</main>',
      width: 300,
      height: 250,
    });
    const acceptedAttempt = createRenderAttempt({
      artifacts: artifactStore,
      owner: artifactOwner.value,
      prepareRenderSource: () => acceptedSource,
      reservations: reservationService,
    });
    if (!acceptedAttempt.ok) throw new Error(acceptedAttempt.reason);
    const disposeAcceptedArtifact = vi.fn();
    const acceptedArtifact = Object.freeze({
      kind: 'direct_iframe' as const,
      attemptId: acceptedAttempt.value.id,
      slot: acceptedAttempt.value.slot,
      navigationGeneration: acceptedAttempt.value.navigationGeneration,
      dispose: disposeAcceptedArtifact,
    });
    expect(
      acceptedAttempt.value.admitDirectWinner(acceptedSource, Object.freeze({ selectedCpm: 1 }))
    ).toBe(true);
    expect(acceptedAttempt.value.beginDirect()).toBe(true);
    expect(acceptedAttempt.value.beginAdm(acceptedArtifact)).toBe(true);
    expect(acceptedAttempt.value.accept()).toBe(true);
    expect(artifactStore.current('accepted-artifact-slot')).toBe(acceptedArtifact);

    projection.auction.auctionId = 'publisher-mutated';
    expect(
      (
        session?.currentNavigation?.currentAuctionProjection as {
          auction: { auctionId: string };
        }
      ).auction.auctionId
    ).toBe('initial');
    const replacement = session?.replaceNavigation();
    expect(replacement).toMatchObject({ ok: true });
    if (!replacement?.ok) throw new Error('Expected SPA navigation');
    expect(disposeAcceptedArtifact).toHaveBeenCalledOnce();
    expect(artifactStore.current('accepted-artifact-slot')).toBeUndefined();
    expect(replacement.value.currentAuctionProjection).toBeUndefined();
    expect(composition.runtimeSessionForTest()).toBe(session);
    expect(composition.reservationServiceForTest()).toBe(reservationService);

    const pageBids = composition.pageBidsControllerForTest();
    expect(
      pageBids?.commit({
        version: 1,
        auction: {
          version: 1,
          auctionId: 'spa',
          results: [{ slot: 'spa-slot', outcome: 'no_bid' }],
        },
        slots: [browserSlotPlacement('spa-slot')],
        bids: [],
      })
    ).toEqual({ status: 'committed' });
    expect(composition.projectionSlotsForTest()).toEqual(['spa-slot']);

    composition.runtime.dispose();
    expect(session?.disposed).toBe(true);
    expect(slotService?.snapshotForTest()).toEqual({
      cycles: 0,
      intents: 0,
      physicalSlots: 0,
      records: 0,
    });
    expect(targetingService?.snapshotForTest()).toEqual({ frames: 0, slots: 0 });
    expect(reservationService?.snapshotInventoryForTest()).toMatchObject({
      disposed: true,
      size: 0,
    });
    expect(rendererNonces?.snapshotForTest()).toMatchObject({ disposed: true });
    expect(composition.slotServiceForTest()).toBeUndefined();
    expect(composition.targetingServiceForTest()).toBeUndefined();
    expect(composition.reservationServiceForTest()).toBeUndefined();
    expect(composition.rendererNonceRegistryForTest()).toBeUndefined();
  });

  it('commits canonical page-bids into a replacement navigation without mutating boot', async () => {
    const nativeReplaceState = history.replaceState.bind(history);
    const releaseId = 'a'.repeat(64);
    const initialProjection = {
      version: 1,
      auction: {
        version: 1,
        auctionId: 'initial',
        results: [{ slot: 'initial-slot', outcome: 'no_bid' }],
      },
      slots: [browserSlotPlacement('initial-slot')],
      bids: [],
    };
    const spaProjection = {
      version: 1,
      auction: {
        version: 1,
        auctionId: 'spa-auction',
        results: [{ slot: 'spa-slot', outcome: 'no_bid' }],
      },
      slots: [browserSlotPlacement('spa-slot')],
      bids: [],
    };
    const fetchPageBids = vi.fn(async () => ({
      ok: true,
      json: async () => spaProjection,
    }));
    const target: Record<string, unknown> = {};
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest: runtimeManifest(releaseId, ['gpt']),
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: {
          auctionProjection: initialProjection,
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        pageBidsFetcherForTest: fetchPageBids,
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      const boot = (target as { boot: Readonly<{ auctionProjection: object }> }).boot;
      const initialNavigation = composition.runtimeSessionForTest()?.currentNavigation;

      history.pushState({}, '', '/spa-route?section=one');
      await vi.waitFor(() => expect(fetchPageBids).toHaveBeenCalledOnce());
      expect(fetchPageBids).toHaveBeenCalledWith(
        '/_ts/page-bids?path=%2Fspa-route%3Fsection%3Done',
        expect.objectContaining({
          credentials: 'include',
          headers: { 'X-TSJS-Page-Bids': '1' },
          signal: expect.any(AbortSignal),
        })
      );
      await vi.waitFor(() =>
        expect(
          composition.runtimeSessionForTest()?.currentNavigation?.currentAuctionProjection
        ).toMatchObject({ auction: { auctionId: 'spa-auction' } })
      );

      expect(composition.runtimeSessionForTest()?.currentNavigation).not.toBe(initialNavigation);
      expect(initialNavigation?.disposed).toBe(true);
      expect(composition.projectionSlotsForTest()).toEqual(['spa-slot']);
      expect(boot.auctionProjection).toMatchObject({ auction: { auctionId: 'initial' } });
      expect(Object.isFrozen(boot.auctionProjection)).toBe(true);

      history.replaceState({}, '', '/spa-replaced');
      await vi.waitFor(() => expect(fetchPageBids).toHaveBeenCalledTimes(2));
      expect(fetchPageBids).toHaveBeenLastCalledWith(
        '/_ts/page-bids?path=%2Fspa-replaced',
        expect.objectContaining({ signal: expect.any(AbortSignal) })
      );

      nativeReplaceState({}, '', '/spa-popped');
      window.dispatchEvent(new PopStateEvent('popstate'));
      await vi.waitFor(() => expect(fetchPageBids).toHaveBeenCalledTimes(3));
      window.dispatchEvent(new PopStateEvent('popstate'));
      await Promise.resolve();
      expect(fetchPageBids).toHaveBeenCalledTimes(3);

      fetchPageBids.mockResolvedValueOnce({
        ok: false,
        json: async () => spaProjection,
      });
      history.pushState({}, '', '/spa-retry');
      await vi.waitFor(() => expect(fetchPageBids).toHaveBeenCalledTimes(4));
      history.replaceState({}, '', '/spa-retry');
      await vi.waitFor(() => expect(fetchPageBids).toHaveBeenCalledTimes(5));
      expect(fetchPageBids).toHaveBeenLastCalledWith(
        '/_ts/page-bids?path=%2Fspa-retry',
        expect.objectContaining({ signal: expect.any(AbortSignal) })
      );
    } finally {
      composition.runtime.dispose();
      history.replaceState({}, '', '/');
    }
  });

  it('publishes a committed page-bids winner through the replacement navigation GPT lifecycle', async () => {
    const releaseId = 'a'.repeat(64);
    const gpt = synchronousGptAdapter();
    const placement = browserSlotPlacement('spa-winner');
    const bid = {
      candidateId: 'BBBBBBBBBBBB',
      slot: placement.slot,
      provider: 'trusted',
      upstreamBidId: 'spa-upstream',
      cpm: 2,
      currency: 'USD' as const,
      targeting: { hb_bidder: 'trusted' },
      rendererReservationId: `r1_${'s'.repeat(22)}`,
      renderSource: {
        type: 'adm' as const,
        version: 1 as const,
        adm: '<main>spa winner</main>',
        width: 300,
        height: 250,
      },
    };
    const spaProjection = {
      version: 1,
      auction: {
        version: 1,
        auctionId: 'spa-production',
        results: [
          { slot: placement.slot, outcome: 'winner' as const, candidateId: bid.candidateId },
        ],
      },
      slots: [placement],
      bids: [bid],
    };
    const fetchPageBids = vi.fn(async () => ({ ok: true, json: async () => spaProjection }));
    const element = document.createElement('div');
    element.id = placement.divId;
    document.body.append(element);
    let prefix = 0;
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId,
        manifest: runtimeManifest(releaseId, ['gpt']),
        knownIntegrationIds: Object.freeze(['gpt']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial-empty', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: () => ({ config: Object.freeze({}), interfaces: Object.freeze({}) }),
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: gpt.adapter,
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners: vi.fn() },
        createIdentityIssuerForTest: () => {
          prefix += 1;
          return createTestNavigationIdentityIssuer({
            getRandomValues: (target) => {
              target.fill(prefix);
              return target;
            },
          });
        },
        pageBidsFetcherForTest: fetchPageBids,
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(createGptIntegrationRegistration(releaseId))
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      history.pushState({}, '', '/spa-production');
      await vi.waitFor(() => expect(fetchPageBids).toHaveBeenCalledOnce());
      await vi.waitFor(() => expect(gpt.physicalSlots()).toHaveLength(1));
      const physicalSlot = gpt.physicalSlots()[0];
      expect(gpt.display).toHaveBeenCalledExactlyOnceWith(physicalSlot);
      expect(gpt.targetingFor(physicalSlot!)).toEqual(
        new Map([
          ['hb_adid', [bid.rendererReservationId]],
          ['hb_bidder', ['trusted']],
        ])
      );
      expect(
        composition.runtimeSessionForTest()?.currentNavigation?.currentAuctionProjection
      ).toMatchObject({ auction: { auctionId: 'spa-production' } });
    } finally {
      composition.runtime.dispose();
      element.remove();
    }
  });

  it('unwinds a lazily-created session when navigation identity generation fails', async () => {
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: runtimeManifest('a'.repeat(64), []),
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        coreActivations: {
          correctnessGptListeners: vi.fn(),
        },
        createIdentityIssuerForTest: () => ({
          ok: false,
          reason: 'identity_generation_failed',
        }),
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(composition.runtimeSessionForTest()).toBeUndefined();
    expect(composition.projectionSlotsForTest()).toBeUndefined();
    expect(composition.pucBridgeForTest()).toBeUndefined();
  });

  it('falls back before publishing services when the PUC capture listener cannot install', async () => {
    const correctnessGptListeners = vi.fn();
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: runtimeManifest('a'.repeat(64), []),
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(() => undefined),
          prebid: fakePrebidAdapter(),
        },
        coreActivations: { correctnessGptListeners },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(correctnessGptListeners).not.toHaveBeenCalled();
    expect(composition.pucBridgeForTest()).toBeUndefined();
    expect(composition.slotServiceForTest()).toBeUndefined();
  });

  it('releases initial programmatic slots before admitting a replacement SPA projection', async () => {
    let prefix = 0;
    const programmaticSlots = Object.freeze(
      Array.from({ length: 256 }, (_, index) => `programmatic-${index}`)
    );
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: runtimeManifest('a'.repeat(64), []),
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        admittedProgrammaticSlotsForTest: programmaticSlots,
        coreActivations: {
          correctnessGptListeners: vi.fn(),
        },
        createIdentityIssuerForTest: () => {
          prefix += 1;
          return createTestNavigationIdentityIssuer({
            getRandomValues: (target) => {
              target.fill(prefix);
              return target;
            },
          });
        },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(composition.projectionSlotsForTest()).toEqual(programmaticSlots);
    const replacement = composition.runtimeSessionForTest()?.replaceNavigation();
    expect(replacement).toMatchObject({ ok: true });
    expect(composition.projectionSlotsForTest()).toEqual([]);

    expect(
      composition.pageBidsControllerForTest()?.commit({
        version: 1,
        auction: {
          version: 1,
          auctionId: 'spa',
          results: [{ slot: 'spa-slot', outcome: 'no_bid' }],
        },
        slots: [browserSlotPlacement('spa-slot')],
        bids: [],
      })
    ).toEqual({ status: 'committed' });
    expect(composition.projectionSlotsForTest()).toEqual(['spa-slot']);
  });

  it('fails closed when admitted programmatic input contains duplicate slot ids', async () => {
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: runtimeManifest('a'.repeat(64), []),
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        admittedProgrammaticSlotsForTest: Object.freeze(['duplicate', 'duplicate']),
        coreActivations: {
          correctnessGptListeners: vi.fn(),
        },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(composition.runtimeSessionForTest()).toBeUndefined();
    expect(composition.projectionSlotsForTest()).toBeUndefined();
  });

  it.each([
    [2, 255],
    [1, 256],
  ] as const)(
    'rejects one atomic initial registration of %i server plus %i programmatic records',
    async (serverCount, programmaticCount) => {
      const composition = createTestBrowserRuntimeComposition(
        {
          target: {},
          releaseId: 'a'.repeat(64),
          manifest: runtimeManifest('a'.repeat(64), []),
          knownIntegrationIds: Object.freeze([]),
          boot: {
            auctionProjection: {
              version: 1,
              auction: {
                version: 1,
                auctionId: 'initial',
                results: Array.from({ length: serverCount }, (_, index) => ({
                  outcome: 'no_bid' as const,
                  slot: `server-${index}`,
                })),
              },
              slots: Array.from({ length: serverCount }, (_, index) =>
                browserSlotPlacement(`server-${index}`)
              ),
              bids: [],
            },
            creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
            diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
          },
          kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
        },
        {
          admittedProgrammaticSlotsForTest: Object.freeze(
            Array.from({ length: programmaticCount }, (_, index) => `programmatic-${index}`)
          ),
          coreActivations: {
            correctnessGptListeners: vi.fn(),
          },
        }
      );

      expect(composition.runtime.start()).toBe(true);
      await expect(composition.runtime.install()).resolves.toEqual({
        state: 'fallback',
        reason: 'bundle_partial',
      });
      expect(composition.runtimeSessionForTest()).toBeUndefined();
      expect(composition.slotServiceForTest()).toBeUndefined();
      expect(composition.projectionSlotsForTest()).toBeUndefined();
    }
  );

  it('owns an immutable copy of admitted programmatic slot input for navigation cleanup', async () => {
    const programmaticSlots = ['programmatic-one', 'programmatic-two'];
    const composition = createTestBrowserRuntimeComposition(
      {
        target: {},
        releaseId: 'a'.repeat(64),
        manifest: runtimeManifest('a'.repeat(64), []),
        knownIntegrationIds: Object.freeze([]),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'initial', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        admittedProgrammaticSlotsForTest: programmaticSlots,
        coreActivations: {
          correctnessGptListeners: vi.fn(),
        },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    programmaticSlots[0] = 'publisher-mutated';
    programmaticSlots.length = 1;

    expect(composition.runtimeSessionForTest()?.replaceNavigation()).toMatchObject({ ok: true });
    expect(composition.projectionSlotsForTest()).toEqual([]);
  });

  it('constructs or activates nothing after a terminal fallback', async () => {
    vi.useFakeTimers();
    const serviceConstruction = vi.fn(() => ({
      config: Object.freeze({}),
      interfaces: Object.freeze({}),
    }));
    const adapterActivation = vi.fn(() => 'pending' as const);
    const listenerActivation = vi.fn(() => vi.fn());
    const latePreparation = vi.fn();
    const target = {};
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId: 'a'.repeat(64),
        manifest: runtimeManifest('a'.repeat(64), ['missing']),
        knownIntegrationIds: Object.freeze(['missing']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: { version: 1, auctionId: 'boot', results: [] },
            slots: [],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        getBindings: serviceConstruction,
        kernel: {
          addAdUnits: vi.fn(),
          diagnostics: Object.freeze({}),
          requestAds: vi.fn(),
        },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(adapterActivation),
          prebid: fakePrebidAdapter(adapterActivation),
          messaging: fakeMessagingAdapter(listenerActivation),
        },
        coreActivations: {
          correctnessGptListeners: adapterActivation,
        },
      }
    );

    expect(composition.runtime.start()).toBe(true);
    const installed = composition.runtime.install();
    await vi.advanceTimersByTimeAsync(10_000);
    await expect(installed).resolves.toEqual({
      state: 'fallback',
      reason: 'bundle_partial',
    });
    expect(composition.runtimeSessionForTest()).toBeUndefined();
    expect(composition.projectionSlotsForTest()).toBeUndefined();
    expect(vi.getTimerCount()).toBe(0);

    expect(
      (target as { _registerIntegration(value: unknown): boolean })._registerIntegration(
        Object.freeze({
          abi: 1,
          id: 'missing',
          phase: 'takeover',
          releaseId: 'a'.repeat(64),
          prepare: latePreparation,
        })
      )
    ).toBe(false);
    await vi.runAllTimersAsync();

    expect(serviceConstruction).not.toHaveBeenCalled();
    expect(adapterActivation).not.toHaveBeenCalled();
    expect(listenerActivation).not.toHaveBeenCalled();
    expect(latePreparation).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('isolates and locally logs a throwing auction-context contributor', async () => {
    const releaseId = 'a'.repeat(64);
    const target = {};
    const requestConfigs: unknown[] = [];
    const auctionFetcher = vi.fn(async (_input: string, init: RequestInit) => {
      const body = JSON.parse(String(init.body)) as { config: unknown };
      requestConfigs.push(body.config);
      return {
        ok: true,
        json: async () => ({
          id: 'context-auction',
          cur: 'USD',
          seatbid: [],
          ext: {
            trusted_server: {
              slot_results: {
                version: 1,
                auctionId: 'context-auction',
                results: [{ slot: 'server-slot', outcome: 'no_bid' }],
              },
            },
          },
        }),
      };
    });
    const warn = vi.spyOn(localLog, 'warn').mockImplementation(() => undefined);
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        manifest: runtimeManifest(releaseId, ['context_test']),
        knownIntegrationIds: Object.freeze(['context_test']),
        boot: {
          auctionProjection: {
            version: 1,
            auction: {
              version: 1,
              auctionId: 'initial',
              results: [{ slot: 'server-slot', outcome: 'no_bid' }],
            },
            slots: [browserSlotPlacement('server-slot')],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: false, gpt: { active: false } },
        },
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        auctionFetcherForTest: auctionFetcher,
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );

    try {
      expect(composition.runtime.start()).toBe(true);
      expect(
        composition.runtime.registerIntegration(
          testTakeoverRegistration('context_test', releaseId, () =>
            Object.freeze({ activate: vi.fn() })
          )
        )
      ).toBe(true);
      await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
      expect((target as { log?: unknown }).log).toBe(publicLog);

      const registry = composition.auctionContextRegistryForTest();
      const session = composition.runtimeSessionForTest();
      expect(registry).toBeDefined();
      expect(session).toBeDefined();
      expect(
        registry?.register(
          'context_test',
          () => {
            throw new Error('publisher contributor');
          },
          session!
        )
      ).toBe(true);

      const api = target as {
        requestAds(options?: unknown): Promise<{ readonly slots: readonly object[] }>;
      };
      await expect(api.requestAds({ slots: ['server-slot'] })).resolves.toEqual({
        slots: [{ slot: 'server-slot', path: 'primary', outcome: 'no_bid' }],
      });
      const diagnostics = target as {
        diagnostics?: { renderTrace?: { history(): readonly unknown[] } };
      };
      expect(diagnostics.diagnostics?.renderTrace?.history()).toEqual([]);

      expect(requestConfigs).toEqual([{}]);
      expect(warn).toHaveBeenCalledExactlyOnceWith('auction context: contributor failed', {
        integrationId: 'context_test',
        reason: 'contributor_failed',
      });
    } finally {
      composition.runtime.dispose();
      warn.mockRestore();
    }
  });

  it('exercises transactional addAdUnits and invocation-time requestAds snapshots through the test kernel', async () => {
    const releaseId = 'a'.repeat(64);
    const integrationIds = Object.freeze([
      BROWSER_TEST_TRACE_PROVIDER_ID,
      'context_test',
      'diagnostics_presentation',
    ]);
    const catalogIds = Object.freeze([
      BROWSER_TEST_TRACE_PROVIDER_ID,
      BROWSER_TEST_OPTIONAL_GPT_DIAG_PROVIDER_ID,
      'context_test',
      'diagnostics_presentation',
    ]);
    const manifest = runtimeManifest(releaseId, integrationIds);
    const runtimeScript = document.createElement('script');
    runtimeScript.id = 'trustedserver-js';
    runtimeScript.src = new URL(manifest.runtimeSrc, window.location.origin).href;
    document.head.append(runtimeScript);
    let executingScript: HTMLScriptElement | null = runtimeScript;
    const currentScript = vi
      .spyOn(document, 'currentScript', 'get')
      .mockImplementation(() => executingScript);
    const frames: FrameRequestCallback[] = [];
    const idle: Array<() => void> = [];
    const presentationAnimationFrame = vi.fn();
    vi.stubGlobal('requestAnimationFrame', presentationAnimationFrame);
    const target = {};
    const preGateSlot = document.createElement('div');
    preGateSlot.id = 'pre-gate-overlay-slot';
    document.body.append(preGateSlot);
    const requestBodies: Array<{
      adUnits: Array<{ code: string }>;
      config: Readonly<Record<string, unknown>>;
    }> = [];
    const auctionFetcher = vi.fn(async (_input: string, init: RequestInit) => {
      const body = JSON.parse(String(init.body)) as {
        adUnits: Array<{ code: string }>;
        config: Readonly<Record<string, unknown>>;
      };
      requestBodies.push(body);
      const slots = body.adUnits.map(({ code }) => code);
      const winnerSlot =
        requestBodies.length === 1 || (slots.length === 1 && slots[0] === 'ambiguous-slot')
          ? slots[0]
          : undefined;
      const candidateId = 'AAAAAAAAAAAA';
      const renderSource = {
        type: 'adm',
        version: 1,
        adm: '<div>programmatic winner</div>',
        width: 300,
        height: 250,
      } as const;
      return {
        ok: true,
        json: async () => ({
          id: `auction-${requestBodies.length}`,
          cur: 'USD',
          seatbid: winnerSlot
            ? [
                {
                  seat: 'fictional',
                  bid: [
                    {
                      id: 'r1_AAAAAAAAAAAAAAAAAAAAAA',
                      impid: winnerSlot,
                      price: 1,
                      adm: renderSource.adm,
                      w: renderSource.width,
                      h: renderSource.height,
                      ext: {
                        trusted_server: {
                          candidate_id: candidateId,
                          slot_id: winnerSlot,
                          render_source: renderSource,
                        },
                      },
                    },
                  ],
                },
              ]
            : [],
          ext: {
            trusted_server: {
              slot_results: {
                version: 1,
                auctionId: `auction-${requestBodies.length}`,
                results: slots.map((slot) =>
                  slot === winnerSlot
                    ? { slot, outcome: 'winner', candidateId }
                    : { slot, outcome: 'no_bid' }
                ),
              },
            },
          },
        }),
      };
    });
    const composition = createTestBrowserRuntimeComposition(
      {
        target,
        releaseId,
        document,
        manifest,
        knownIntegrationIds: catalogIds,
        catalog: runtimeCatalog(catalogIds),
        boot: {
          auctionProjection: {
            version: 1,
            auction: {
              version: 1,
              auctionId: 'initial',
              results: [{ slot: 'server-slot', outcome: 'no_bid' }],
            },
            slots: [browserSlotPlacement('server-slot')],
            bids: [],
          },
          creative: { version: 1, enabled: false, clickGuard: false, renderGuard: false },
          diagnostics: { version: 1, renderTraceOverlay: true, gpt: { active: false } },
        },
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
        kernel: { addAdUnits: vi.fn(), diagnostics: Object.freeze({}), requestAds: vi.fn() },
      },
      {
        adapters: {
          googletag: fakeGoogletagAdapter(),
          messaging: fakeMessagingAdapter(),
          prebid: fakePrebidAdapter(),
        },
        auctionFetcherForTest: auctionFetcher,
        coreActivations: { correctnessGptListeners: vi.fn() },
      }
    );
    const originalHeadAppend = document.head.append.bind(document.head);
    const loadedPresentation = vi.fn();
    const headAppend = vi.spyOn(document.head, 'append').mockImplementation((...nodes) => {
      originalHeadAppend(...nodes);
      for (const node of nodes) {
        if (!(node instanceof HTMLScriptElement) || node === runtimeScript) continue;
        expect(node.src).toBe(
          new URL(
            manifest.integrations.find(({ id }) => id === 'diagnostics_presentation')!.src!,
            window.location.origin
          ).href
        );
        executingScript = node;
        expect(
          composition.runtime.registerIntegration(
            createDiagnosticsPresentationIntegrationRegistration(releaseId)
          )
        ).toBe(true);
        loadedPresentation();
        node.onload?.(new Event('load'));
        executingScript = runtimeScript;
      }
    });

    expect(composition.runtime.start()).toBe(true);
    expect(
      composition.runtime.registerIntegration(
        composition.createTraceCapabilityProviderRegistrationForTest()
      )
    ).toBe(true);
    expect(
      composition.runtime.registerIntegration(
        testTakeoverRegistration('context_test', releaseId, () =>
          Object.freeze({ activate: vi.fn() })
        )
      )
    ).toBe(true);
    await expect(composition.runtime.install()).resolves.toMatchObject({ state: 'kernel' });
    expect(document.getElementById(TRACE_PANEL_ID)).toBeNull();
    expect(presentationAnimationFrame).not.toHaveBeenCalled();
    expect(frames).toEqual([]);
    expect(idle).toEqual([]);
    expect(loadedPresentation).not.toHaveBeenCalled();
    expect(preGateSlot.getAttributeNames().filter((name) => name.startsWith('data-ts-'))).toEqual(
      []
    );
    expect(composition.runtimeSessionForTest()?.interfaces).not.toHaveProperty('gpt.events.v1');
    expect(composition.runtimeSessionForTest()?.interfaces).not.toHaveProperty('gpt_diag.v1');
    expect(composition.runtime.protectFirstDisplayAttemptBatch([Promise.resolve()])).toBe(true);
    await Promise.resolve();
    await Promise.resolve();
    frames.shift()?.(1);
    frames.shift()?.(2);
    idle.shift()?.();
    await vi.waitFor(() => expect(loadedPresentation).toHaveBeenCalledOnce());
    expect(document.getElementById(TRACE_PANEL_ID)).not.toBeNull();
    expect(presentationAnimationFrame).not.toHaveBeenCalled();
    const contextContributor = vi.fn(() => ({ page: 'context' }));
    const session = composition.runtimeSessionForTest();
    expect(session).toBeDefined();
    expect(
      composition
        .auctionContextRegistryForTest()
        ?.register('context_test', contextContributor, session!)
    ).toBe(true);
    const api = target as {
      addAdUnits(value: unknown): { readonly registered: readonly string[] };
      requestAds(options?: unknown): Promise<{ readonly slots: readonly object[] }>;
    };
    const programmatic = {
      code: 'programmatic-slot',
      mediaTypes: { banner: { sizes: [[300, 250]] } },
      bids: [{ bidder: 'fictional', params: { placement: 7 } }],
    };

    expect(api.addAdUnits(programmatic)).toEqual({ registered: ['programmatic-slot'] });
    expect(composition.projectionSlotsForTest()).toEqual(['server-slot', 'programmatic-slot']);
    const slotService = composition.slotServiceForTest();
    expect(slotService?.resolveRegisteredSlot('programmatic-slot')).toMatchObject({
      domAliases: [],
      registeredSlotId: 'programmatic-slot',
      source: 'programmatic',
    });
    expect(slotService?.resolveDomAlias('programmatic-slot')).toBeUndefined();
    expect(() =>
      api.addAdUnits([
        {
          code: 'must-roll-back',
          mediaTypes: { banner: { sizes: [[1, 1]] } },
        },
        {
          code: 'server-slot',
          mediaTypes: { banner: { sizes: [[1, 1]] } },
        },
      ])
    ).toThrowError(expect.objectContaining({ code: 'slot_collision', unitIndex: 1 }));
    expect(composition.projectionSlotsForTest()).toEqual(['server-slot', 'programmatic-slot']);
    document.body.insertAdjacentHTML(
      'beforeend',
      '<div id="programmatic-slot"><span>placeholder</span></div>'
    );
    const explicit = api.requestAds({ slots: ['unknown', 'programmatic-slot'] });
    await vi.waitFor(() =>
      expect(document.querySelector('#programmatic-slot iframe')).not.toBeNull()
    );
    const frame = document.querySelector<HTMLIFrameElement>('#programmatic-slot iframe');
    expect(frame?.srcdoc).toContain('programmatic winner');
    frame?.dispatchEvent(new Event('load'));
    await expect(explicit).resolves.toEqual({
      slots: [
        { slot: 'unknown', path: 'primary', outcome: 'failed', reason: 'slot_unresolved' },
        { slot: 'programmatic-slot', path: 'primary', outcome: 'accepted' },
      ],
    });
    const renderTrace = (
      target as {
        diagnostics?: {
          renderTrace?: {
            current(): Readonly<Record<string, Readonly<Record<string, unknown>>>>;
            history(): readonly Readonly<Record<string, unknown>>[];
          };
        };
      }
    ).diagnostics?.renderTrace;
    expect(renderTrace?.current()['programmatic-slot']).toEqual(
      expect.objectContaining({
        slotId: 'programmatic-slot',
        path: 'auction',
        rendered: true,
        injected: true,
        elementId: 'programmatic-slot',
        servedFrom: 'inline',
        count: 1,
      })
    );
    expect(renderTrace?.history()).toHaveLength(1);
    expect(Object.isFrozen(renderTrace?.history()[0])).toBe(true);
    const programmaticSlot = document.getElementById('programmatic-slot');
    await vi.waitFor(() => {
      expect(programmaticSlot?.getAttribute('data-ts-rendered')).toBe('true');
      expect(programmaticSlot?.getAttribute('data-ts-injected')).toBe('true');
      expect(document.getElementById(TRACE_PANEL_ID)?.textContent).toContain('programmatic-slot');
    });
    expect(target).not.toHaveProperty('renders');
    expect(target).not.toHaveProperty('renderLog');
    expect(target).not.toHaveProperty('renderSeq');
    expect(requestBodies[0]).toEqual({
      adUnits: [programmatic],
      config: { page: 'context' },
    });
    expect(contextContributor).toHaveBeenCalledOnce();

    const omitted = api.requestAds();
    expect(
      api.addAdUnits({
        code: 'later-slot',
        mediaTypes: { banner: { sizes: [[728, 90]] } },
      })
    ).toEqual({ registered: ['later-slot'] });
    await expect(omitted).resolves.toEqual({
      slots: [
        { slot: 'server-slot', path: 'primary', outcome: 'no_bid' },
        { slot: 'programmatic-slot', path: 'primary', outcome: 'no_bid' },
      ],
    });
    expect(requestBodies[1]?.adUnits.map(({ code }) => code)).toEqual([
      'server-slot',
      'programmatic-slot',
    ]);
    expect(requestBodies[1]?.adUnits).not.toContainEqual(
      expect.objectContaining({ code: 'later-slot' })
    );
    expect(requestBodies[1]?.config).toEqual({ page: 'context' });
    expect(contextContributor).toHaveBeenCalledTimes(2);
    expect(auctionFetcher).toHaveBeenCalledTimes(2);

    expect(
      slotService?.register(session!.currentNavigation!, [
        {
          adUnitCode: '/network/path',
          domAliases: ['publisher-alias'],
          registeredSlotId: 'alias-owner',
          source: 'server',
        },
      ])
    ).toMatchObject({ ok: true });
    await expect(
      api.requestAds({ slots: ['publisher-alias', '/network/path', 'alias-owner'] })
    ).resolves.toEqual({
      slots: [
        { slot: 'publisher-alias', path: 'primary', outcome: 'failed', reason: 'slot_unresolved' },
        { slot: '/network/path', path: 'primary', outcome: 'failed', reason: 'slot_unresolved' },
        { slot: 'alias-owner', path: 'primary', outcome: 'no_bid' },
      ],
    });
    expect(requestBodies[2]?.adUnits.map(({ code }) => code)).toEqual(['alias-owner']);

    expect(
      api.addAdUnits({
        code: 'ambiguous-slot',
        mediaTypes: { banner: { sizes: [[300, 250]] } },
      })
    ).toEqual({ registered: ['ambiguous-slot'] });
    document.body.insertAdjacentHTML(
      'beforeend',
      '<div id="ambiguous-slot"></div><div id="ambiguous-slot"></div>'
    );
    await expect(api.requestAds({ slots: ['ambiguous-slot'] })).resolves.toEqual({
      slots: [
        {
          slot: 'ambiguous-slot',
          path: 'primary',
          outcome: 'failed',
          reason: 'slot_unresolved',
        },
      ],
    });
    expect(document.querySelectorAll('[id="ambiguous-slot"] iframe')).toHaveLength(0);
    expect(contextContributor).toHaveBeenCalledTimes(4);
    expect(auctionFetcher).toHaveBeenCalledTimes(4);

    session?.currentNavigation?.dispose();
    expect(renderTrace?.current()).toEqual({});
    await vi.waitFor(() => expect(programmaticSlot?.hasAttribute('data-ts-rendered')).toBe(false));

    composition.runtime.dispose();
    expect(document.getElementById(TRACE_PANEL_ID)).toBeNull();
    expect(preGateSlot.getAttributeNames().filter((name) => name.startsWith('data-ts-'))).toEqual(
      []
    );
    expect(() => api.addAdUnits(programmatic)).toThrowError(
      expect.objectContaining({ name: 'AdUnitRegistrationError', code: 'slot_collision' })
    );
    document.body.innerHTML = '';
    headAppend.mockRestore();
    currentScript.mockRestore();
    runtimeScript.remove();
    preGateSlot.remove();
    vi.unstubAllGlobals();
  });
});
