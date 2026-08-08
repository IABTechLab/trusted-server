import {
  createBrowserGoogletagAdapter,
  createNoopGoogletagAdapter,
  type GoogletagAdapter,
  type GoogletagGlobalTarget,
} from '../adapters/googletag';
import {
  createBrowserMessagingAdapter,
  createNoopMessagingAdapter,
  type MessageEventTarget,
  type MessagingAdapter,
  type MessagingValidationOptions,
} from '../adapters/messaging';
import {
  createBrowserPrebidAdapter,
  createNoopPrebidAdapter,
  type PrebidAdapter,
  type PrebidGlobalTarget,
  type PrebidTrustedServerAuctionV1,
} from '../adapters/prebid';
import { parseCacheFetchPolicyV1 } from '../core/config';
import { parseTrustedServerAuctionResponseV1 } from '../core/auction';
import type {
  BootManifestV1,
  BrowserAuctionProjectionV1,
  CreativeBootV1,
  DiagnosticsBootV1,
} from '../core/types';
import {
  createRenderTrace,
  isEffectivelyVisible,
  type RenderTraceRuntimeOwner,
} from '../core/trace';
import {
  parseBidRenderSourceV1,
  parseBrowserAuctionProjectionV1,
} from '../core/contracts/auction_projection';
import { validateApsRenderer } from '../core/contracts/aps_renderer';
import { validateRequestAdsOptions } from '../core/contracts/request_ads';
import { log } from '../core/log';
import {
  AdUnitRegistrationError,
  addAdUnitsResult,
  prepareProgrammaticAdUnits,
  serializeAuctionRequestBody,
} from '../core/registry';
import { prepareAdmIframe } from '../core/render';
import { APS_RENDERER_V1_PATH, renderDirectApsAttempt } from '../integrations/aps/render';
import { installClickGuard } from '../integrations/creative/click';
import { installDynamicIframeProxy } from '../integrations/creative/iframe';
import { installDynamicImageProxy } from '../integrations/creative/image';
import { createCreativeStartup } from '../integrations/creative/startup';
import { createDataDomeRuntime } from '../integrations/datadome/module';
import { createDidomiRuntime } from '../integrations/didomi/module';
import { createGoogleTagManagerRuntime } from '../integrations/google_tag_manager/module';
import {
  publishGptWinner,
  startGptSlotOperation,
  type GptSlotOperationInput,
  type GptWinnerPublicationInput,
  type GptWinnerPublicationResult,
} from '../integrations/gpt/module';
import { createGptStartup } from '../integrations/gpt/startup';
import {
  activateGptDiagnosticsFactCapture,
  createGptDiagnosticsFactBuffer,
  type GptDiagnosticsFactBuffer,
} from '../integrations/gpt_diagnostics/facts';
import {
  createGptDiagnosticsRuntime,
  type GptDiagnosticsRuntime,
} from '../integrations/gpt_diagnostics';
import {
  createPrebidRefreshPolicy,
  createPrebidSelectionCoordinator,
  createPrebidSyntheticRefreshRunner,
  preparePrebidRegisteredRefreshAuction,
  publishPrebidBid,
  type PrebidSelectionCoordinator,
} from '../integrations/prebid/module';
import { createPrebidStartup } from '../integrations/prebid/startup';
import { createLockrRuntime } from '../integrations/lockr/module';
import { createOsanoRuntime } from '../integrations/osano/module';
import { createPermutiveRuntime } from '../integrations/permutive/module';
import { createSourcepointRuntime } from '../integrations/sourcepoint/module';
import { createTestlightRuntime } from '../integrations/testlight/module';
import { createBrowserNavigationIdentityIssuer } from '../kernel/identity';
import {
  createDiagnosticsBus,
  type DiagnosticsBus,
  type DiagnosticsObservation,
} from '../kernel/diagnostics';
import type {
  NavigationIdentityIssuerFactory,
  RenderAttemptScope,
  RuntimeSession,
} from '../kernel/sessions';
import { createRuntimeSession } from '../kernel/sessions';
import type { CoreActivationContext } from '../kernel/integration_registry';
import { createRuntime, type Runtime, type RuntimeOptions } from '../kernel/runtime';
import {
  createAuctionContextRegistry,
  type AuctionContextContributor,
  type AuctionContextRegistry,
  type ContextContributorOwner,
} from '../services/context';
import {
  createAuctionBatchService,
  type AuctionBatchFetcher,
  type AuctionBatchService,
} from '../services/auction_batch';
import {
  createPageBidsController,
  type PageBidsController,
  prepareInitialAuctionProjection,
} from '../services/projections';
import { createReservationService, type ReservationService } from '../services/reservations';
import {
  createCommittedArtifactStore,
  createRenderAttempt,
  createRendererNonceRegistry,
  resolveCacheAdmAttempt,
  renderDirectCacheAttempt,
  resizeCollapsedPucShell,
  renderDirectAdmAttempt,
  type RenderAttempt,
  type CommittedArtifactStore,
  type RendererNonceRegistry,
  type SlotOperationCreationResult,
} from '../services/render';
import { createPucBridge, type PucBridge, type PucBridgeOptions } from '../services/puc_bridge';
import {
  createBrowserSlotReconciliationBoundary,
  createSlotService,
  type SlotRecord,
  type SlotRegistrationFailure,
  type SlotService,
} from '../services/slots';
import { createTargetingService, type TargetingService } from '../services/targeting';

export interface BrowserAdapters {
  readonly googletag: GoogletagAdapter;
  readonly messaging: MessagingAdapter;
  readonly prebid: PrebidAdapter;
}

export interface BrowserComposition {
  readonly adapters: Readonly<BrowserAdapters>;
}

export interface BrowserServices {
  readonly artifacts: CommittedArtifactStore;
  readonly auctionBatches: AuctionBatchService;
  readonly pucBridge: PucBridge;
  readonly reservations: ReservationService;
  readonly rendererNonces: RendererNonceRegistry;
  readonly renderDirectAdm: (attempt: RenderAttempt, container: HTMLElement) => boolean;
  readonly renderDirectAps: (attempt: RenderAttempt, container: HTMLElement) => boolean;
  readonly renderDirectCache: (attempt: RenderAttempt, container: HTMLElement) => boolean;
  readonly slots: SlotService;
  readonly targeting: TargetingService;
}

export type BrowserAdapterTarget = GoogletagGlobalTarget & PrebidGlobalTarget & MessageEventTarget;

export interface BrowserCompositionOptions {
  readonly adapters?: Partial<BrowserAdapters>;
  readonly messagingValidation?: MessagingValidationOptions;
  readonly target?: BrowserAdapterTarget;
}

export interface BrowserRuntimeComposition extends BrowserComposition {
  readonly runtime: Runtime;
  /** Return the lazily activated session for tests; this is not a `tsjs` field. */
  readonly runtimeSessionForTest: () => RuntimeSession | undefined;
  /** Construct a controller for the current navigation in coordinated-cutover tests. */
  readonly pageBidsControllerForTest: () => PageBidsController | undefined;
  /** Return one frozen slot-id inventory for coordinated-cutover tests. */
  readonly projectionSlotsForTest: () => readonly string[] | undefined;
  /** Return the lazily activated context registry for coordinated-cutover tests. */
  readonly auctionContextRegistryForTest: () => AuctionContextRegistry | undefined;
  /** Return runtime-owned slot operations only in coordinated-cutover tests. */
  readonly slotServiceForTest: () => SlotService | undefined;
  /** Return runtime-owned targeting operations only in coordinated-cutover tests. */
  readonly targetingServiceForTest: () => TargetingService | undefined;
  /** Return runtime-owned reservation operations only in coordinated-cutover tests. */
  readonly reservationServiceForTest: () => ReservationService | undefined;
  /** Return runtime-owned renderer nonces only in coordinated-cutover tests. */
  readonly rendererNonceRegistryForTest: () => RendererNonceRegistry | undefined;
  /** Return the single runtime-owned PUC bridge only in coordinated-cutover tests. */
  readonly pucBridgeForTest: () => PucBridge | undefined;
  /** Join one prospective GPT attempt through the runtime-owned services in tests. */
  readonly startGptSlotOperationForTest: (
    input: Omit<GptSlotOperationInput, 'pucBridge' | 'slots'>
  ) => SlotOperationCreationResult;
  /** Publish one prospective server winner through the ordered GPT transaction in tests. */
  readonly publishGptWinnerForTest: (
    input: Omit<
      GptWinnerPublicationInput,
      'googletag' | 'navigation' | 'pucBridge' | 'reservations' | 'slots' | 'targeting'
    >
  ) => Promise<GptWinnerPublicationResult>;
}

export interface BrowserCoreActivations {
  readonly correctnessGptListeners: (
    context: CoreActivationContext,
    adapters: Readonly<BrowserAdapters>,
    services: Readonly<BrowserServices>
  ) => void;
}

export interface TestBrowserRuntimeCompositionOptions extends BrowserCompositionOptions {
  readonly auctionFetcherForTest?: AuctionBatchFetcher;
  readonly coreActivations: BrowserCoreActivations;
  readonly creativeActivationForTest?: (config: Readonly<CreativeBootV1>) => () => void;
  readonly creativeStartupForTest?: (config: Readonly<CreativeBootV1>) => void;
  readonly createIdentityIssuerForTest?: NavigationIdentityIssuerFactory;
  readonly admittedProgrammaticSlotsForTest?: readonly string[];
  readonly gptStartupForTest?: (config: unknown) => void;
  readonly prebidStartupForTest?: (config: unknown) => void;
  readonly pucSchedulerForTest?: PucBridgeOptions['scheduler'];
}

interface AcceptedBrowserBoot {
  readonly auctionProjection: object;
  readonly cachePolicy?: unknown;
  readonly creative: Readonly<CreativeBootV1>;
  readonly diagnostics: Readonly<DiagnosticsBootV1>;
  readonly didomi?: unknown;
  readonly manifest: Readonly<BootManifestV1>;
  readonly sourcepoint?: unknown;
}

interface PreparedBrowserServices {
  readonly createAttempt: (owner: RenderAttemptScope) => ReturnType<typeof createRenderAttempt>;
  readonly publisherOrigin: string;
  readonly rendererUrl: string;
  readonly resolveCacheAdm: NonNullable<PucBridgeOptions['resolveCacheAdm']>;
  readonly services: Readonly<Omit<BrowserServices, 'pucBridge'>>;
}

function projectionSlots(projection: object): readonly string[] {
  const accepted = projection as {
    readonly auction: { readonly results: readonly { readonly slot: string }[] };
  };
  return Object.freeze(accepted.auction.results.map(({ slot }) => slot));
}

interface ComposedPrebidRefreshConfig {
  readonly clientSideBidders: readonly string[];
  readonly excludedGamAdUnitPathSuffixes: readonly string[];
}

const EMPTY_PREBID_REFRESH_CONFIG: ComposedPrebidRefreshConfig = Object.freeze({
  clientSideBidders: Object.freeze([]),
  excludedGamAdUnitPathSuffixes: Object.freeze([]),
});

function composedPrebidRefreshConfig(candidate: unknown): ComposedPrebidRefreshConfig {
  try {
    if (typeof candidate !== 'object' || candidate === null || Array.isArray(candidate)) {
      return EMPTY_PREBID_REFRESH_CONFIG;
    }
    const strings = (name: string): readonly string[] => {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, name);
      if (!descriptor || !('value' in descriptor) || !Array.isArray(descriptor.value)) {
        return Object.freeze([]);
      }
      const values: string[] = [];
      for (let index = 0; index < descriptor.value.length; index += 1) {
        const value = descriptor.value[index];
        if (typeof value !== 'string') return Object.freeze([]);
        values.push(value);
      }
      return Object.freeze(values);
    };
    return Object.freeze({
      clientSideBidders: strings('clientSideBidders'),
      excludedGamAdUnitPathSuffixes: strings('excludedGamAdUnitPathSuffixes'),
    });
  } catch {
    return EMPTY_PREBID_REFRESH_CONFIG;
  }
}

function composedPrebidRefreshAuction(
  physicalSlots: readonly object[],
  navigation: RuntimeSession['currentNavigation'],
  slots: SlotService,
  config: ComposedPrebidRefreshConfig
): unknown {
  if (!navigation?.isCurrent()) return undefined;
  const records = slots.snapshotRegisteredSlots(navigation);
  if (!records) return undefined;
  const resolved = new Map<object, Readonly<object>>();
  for (let slotIndex = 0; slotIndex < physicalSlots.length; slotIndex += 1) {
    const physicalSlot = physicalSlots[slotIndex];
    if (!physicalSlot) return undefined;
    let matched: SlotRecord | undefined;
    for (let recordIndex = 0; recordIndex < records.length; recordIndex += 1) {
      const record = records[recordIndex];
      if (
        !record ||
        !slots.isBoundGptSlot(navigation.generation, record.registeredSlotId, physicalSlot)
      ) {
        continue;
      }
      if (matched) return undefined;
      matched = record;
    }
    const source = matched?.directAuctionUnit;
    if (!source || !Object.isFrozen(source)) {
      return undefined;
    }
    resolved.set(physicalSlot, source);
  }
  return preparePrebidRegisteredRefreshAuction({
    clientSideBidders: config.clientSideBidders,
    resolveAdUnit: (slot) => resolved.get(slot),
    slots: physicalSlots,
  });
}

function registerScopedContextContributor(
  registry: AuctionContextRegistry,
  runtimeOwner: RuntimeSession,
  integrationId: string,
  contributor: AuctionContextContributor
): (() => void) | undefined {
  let active = true;
  let releaseRegistration: (() => void) | undefined;
  const owner: ContextContributorOwner = Object.freeze({
    generation: Object.freeze({}),
    isCurrent: () => active && runtimeOwner.isCurrent(),
    onDispose: (kind: string, callback: () => void) => {
      if (kind !== 'auction-context-contributor' || !active || releaseRegistration) {
        throw new Error('Auction context contributor disposer is unavailable');
      }
      releaseRegistration = callback;
    },
  });
  if (!registry.register(integrationId, contributor, owner)) {
    active = false;
    releaseRegistration?.();
    return undefined;
  }
  return (): void => {
    if (!active) return;
    active = false;
    const release = releaseRegistration;
    releaseRegistration = undefined;
    release?.();
  };
}

/**
 * Construct concrete browser dependencies in one place.
 *
 * Task 6 keeps this test-only composition disconnected from the shipped core;
 * the coordinated production switch occurs only after the runtime is complete.
 */
export function createBrowserComposition(
  options: BrowserCompositionOptions = {}
): BrowserComposition {
  const defaultValidator = (candidate: unknown): boolean =>
    validateApsRenderer(candidate) !== undefined;
  const browserMessagingValidation = (): MessagingValidationOptions => {
    try {
      const expectedPublisherOrigin = window.location.origin;
      return {
        expectedPublisherOrigin,
        expectedRendererUrl: new URL('/integrations/aps/renderer/v1', expectedPublisherOrigin).href,
        validateApsRenderer: defaultValidator,
        ...options.messagingValidation,
      };
    } catch {
      return { validateApsRenderer: defaultValidator, ...options.messagingValidation };
    }
  };
  const googletag =
    options.adapters?.googletag ??
    (options.target
      ? createBrowserGoogletagAdapter(options.target)
      : createBrowserGoogletagAdapter());
  const messaging =
    options.adapters?.messaging ??
    (options.target
      ? createBrowserMessagingAdapter(options.target, {
          validateApsRenderer: defaultValidator,
          ...options.messagingValidation,
        })
      : createBrowserMessagingAdapter(undefined, browserMessagingValidation()));
  const prebid =
    options.adapters?.prebid ??
    (options.target ? createBrowserPrebidAdapter(options.target) : createBrowserPrebidAdapter());

  return Object.freeze({
    adapters: Object.freeze({ googletag, messaging, prebid }),
  });
}

/** Construct a side-effect-free dependency set for kernel and service tests. */
export function createNoopBrowserComposition(): BrowserComposition {
  return Object.freeze({
    adapters: Object.freeze({
      googletag: createNoopGoogletagAdapter(),
      messaging: createNoopMessagingAdapter(),
      prebid: createNoopPrebidAdapter(),
    }),
  });
}

/**
 * Construct the single runtime only for coordinated-cutover tests.
 *
 * The shipped core remains on its existing bootstrap until Task 19; keeping this
 * explicit prevents an import of the composition module from claiming globals.
 */
export function createTestBrowserRuntimeComposition(
  runtimeOptions: RuntimeOptions,
  compositionOptions: TestBrowserRuntimeCompositionOptions
): BrowserRuntimeComposition {
  const composition = createBrowserComposition(compositionOptions);
  const providedBindings = runtimeOptions.getBindings;
  let browserServices: Readonly<BrowserServices> | undefined;
  let creativeBoot: Readonly<CreativeBootV1> | undefined;
  let diagnosticsBoot: Readonly<DiagnosticsBootV1> | undefined;
  let diagnosticsBus: DiagnosticsBus | undefined;
  let gptDiagnosticsFacts: GptDiagnosticsFactBuffer | undefined;
  let gptDiagnosticsRuntime: GptDiagnosticsRuntime | undefined;
  let renderTrace: RenderTraceRuntimeOwner | undefined;
  const renderTraceSlotsByNavigation = new Map<object, Set<string>>();
  let acceptedBrowserBoot: AcceptedBrowserBoot | undefined;
  const consumeCoreObservation = (observation: DiagnosticsObservation): void => {
    if (
      observation['kind'] !== 'render_attempt' ||
      typeof observation['slotId'] !== 'string' ||
      (observation['path'] !== 'auction' && observation['path'] !== 'ssat') ||
      typeof observation['rendered'] !== 'boolean' ||
      typeof observation['injected'] !== 'boolean'
    ) {
      return;
    }
    const state = observation['state'];
    const terminal = observation['outcome'];
    const terminalRecord =
      typeof terminal === 'object' && terminal !== null
        ? (terminal as Readonly<Record<string, unknown>>)
        : undefined;
    const attributableEmpty =
      state === 'failed' &&
      terminalRecord?.['outcome'] === 'failed' &&
      terminalRecord['reason'] === 'gam_empty';
    if (state !== 'accepted' && !attributableEmpty) return;
    if ((state === 'accepted') !== observation['rendered']) return;
    const servedFrom = observation['servedFrom'];
    if (servedFrom !== undefined && servedFrom !== 'inline' && servedFrom !== 'pbs-cache') return;
    try {
      const slotId = observation['slotId'];
      const slot = browserServices?.slots.resolveRegisteredSlot(slotId);
      const identifiers = slot
        ? new Set([slot.registeredSlotId, ...slot.domAliases])
        : new Set([slotId]);
      const elements = new Set<HTMLElement>();
      if (typeof document !== 'undefined') {
        for (const identifier of identifiers) {
          const element = document.getElementById(identifier);
          if (element instanceof HTMLElement) elements.add(element);
        }
      }
      const element = elements.size === 1 ? [...elements][0] : undefined;
      const optionalString = (name: 'adId' | 'bidId' | 'creativeId'): string | undefined => {
        const value = observation[name];
        return typeof value === 'string' && value !== '' ? value : undefined;
      };
      const adId = optionalString('adId');
      const bidId = optionalString('bidId');
      const creativeId = optionalString('creativeId');
      const navigation = runtimeSession?.currentNavigation;
      if (navigation?.isCurrent()) {
        const tracedSlots = renderTraceSlotsByNavigation.get(navigation.generation) ?? new Set();
        tracedSlots.add(slotId);
        renderTraceSlotsByNavigation.set(navigation.generation, tracedSlots);
      }
      renderTrace?.record({
        slotId,
        path: observation['path'],
        rendered: observation['rendered'],
        injected: observation['injected'],
        ...(element === undefined
          ? {}
          : { elementId: element.id, visible: isEffectivelyVisible(element) }),
        ...(adId === undefined ? {} : { adId }),
        ...(bidId === undefined ? {} : { bidId }),
        ...(creativeId === undefined ? {} : { creativeId }),
        ...(servedFrom === undefined ? {} : { servedFrom }),
      });
    } catch {
      // Render diagnostics never affect the already-committed attempt.
    }
  };
  const diagnosticsForPublish = (): Readonly<object> => {
    const trace = renderTrace;
    if (!trace) throw new Error('Render diagnostics are unavailable');
    const gpt = gptDiagnosticsRuntime?.currentApi();
    if (diagnosticsBoot?.gpt.active && !gpt) {
      throw new Error('GPT diagnostics are unavailable');
    }
    return Object.freeze({ renderTrace: trace.diagnostics, ...(gpt ? { gpt } : {}) });
  };
  const defaultCreativeRuntime =
    typeof document === 'undefined'
      ? Object.freeze({
          activate: (_config: Readonly<CreativeBootV1>) => () => undefined,
          start: (_config: Readonly<CreativeBootV1>) => undefined,
        })
      : createCreativeStartup({
          document,
          installClickGuard: () => installClickGuard(false),
          installDynamicIframeProxy: () => installDynamicIframeProxy(false),
          installDynamicImageProxy: () => installDynamicImageProxy(false),
        });
  const creativeRuntime = Object.freeze({
    activate: compositionOptions.creativeActivationForTest ?? defaultCreativeRuntime.activate,
    start: compositionOptions.creativeStartupForTest ?? defaultCreativeRuntime.start,
  });
  const startGpt = compositionOptions.gptStartupForTest ?? (() => undefined);
  const gptRuntime = createGptStartup({
    googletag: composition.adapters.googletag,
    slots: () => {
      const slots = browserServices?.slots;
      if (!slots) throw new Error('GPT slot service is unavailable');
      return slots;
    },
    start: startGpt,
  });
  const gptIntegrationRuntime = Object.freeze({
    activate: gptRuntime.activate,
    start: gptRuntime.start,
  });
  let runtimeSession: RuntimeSession | undefined;
  let prebidCoordinator: PrebidSelectionCoordinator | undefined;
  let prebidRefreshConfig = EMPTY_PREBID_REFRESH_CONFIG;
  const startPrebid = compositionOptions.prebidStartupForTest ?? (() => undefined);
  const prebidRefreshRunner = createPrebidSyntheticRefreshRunner({
    prebid: composition.adapters.prebid,
    prepareAuction: (slots, navigation) => {
      const slotService = browserServices?.slots;
      if (!slotService) return undefined;
      return composedPrebidRefreshAuction(slots, navigation, slotService, prebidRefreshConfig);
    },
  });
  const prebidRefreshPolicy = createPrebidRefreshPolicy({
    currentNavigation: () => runtimeSession?.currentNavigation,
    excludedGamAdUnitPathSuffixes: () => prebidRefreshConfig.excludedGamAdUnitPathSuffixes,
    googletag: composition.adapters.googletag,
    runSyntheticAuction: prebidRefreshRunner,
  });
  const completePrebidAuction = (auction: Readonly<PrebidTrustedServerAuctionV1>): void => {
    try {
      auction.complete();
    } catch {
      // The private bidder completion boundary cannot escape into publisher code.
    }
  };
  const publishPrebidAuction = (auction: Readonly<PrebidTrustedServerAuctionV1>): void => {
    const navigation = runtimeSession?.currentNavigation;
    const reservations = browserServices?.reservations;
    const coordinator = prebidCoordinator;
    if (!navigation || !reservations || !coordinator || !navigation.isCurrent()) {
      completePrebidAuction(auction);
      return;
    }
    try {
      const projection = navigation.currentAuctionProjection as
        Readonly<BrowserAuctionProjectionV1> | undefined;
      if (!projection || projection.auction.auctionId !== auction.auctionId) return;
      for (let index = 0; index < auction.bids.length; index += 1) {
        const request = auction.bids[index];
        if (!request) continue;
        const winners = projection.auction.results.filter(
          (result) => result.slot === request.adUnitCode && result.outcome === 'winner'
        );
        if (winners.length !== 1) continue;
        const winner = winners[0];
        if (!winner || winner.outcome !== 'winner') continue;
        const bids = projection.bids.filter(
          (bid) => bid.slot === request.adUnitCode && bid.candidateId === winner.candidateId
        );
        if (bids.length !== 1) continue;
        const bid = bids[0];
        if (!bid) continue;
        const publication = publishPrebidBid({
          admitTrustedBid: (preparedBid) =>
            composition.adapters.prebid.admitTrustedBid(preparedBid),
          auctionId: auction.auctionId,
          adUnitCode: request.adUnitCode,
          bid,
          generatedBid: Object.freeze({
            requestId: request.requestId,
            adId: request.requestId,
            cpm: bid.cpm,
            width: bid.renderSource.width,
            height: bid.renderSource.height,
          }),
          navigation,
          reservations,
          trackAdmittedBid: coordinator.track,
        });
        if (
          !publication.ok &&
          (publication.reason === 'prebid_admission_failed' ||
            publication.reason === 'prebid_contract_violation')
        ) {
          coordinator.settlePublicationFailure(
            navigation,
            auction.auctionId,
            request.adUnitCode,
            publication.reason
          );
        }
      }
    } catch {
      // Invalid/stale projection state publishes no Prebid bid.
    } finally {
      completePrebidAuction(auction);
    }
  };
  const prebidRuntime = createPrebidStartup({
    dispose: () => {
      prebidCoordinator?.dispose();
      prebidCoordinator = undefined;
    },
    onAuction: publishPrebidAuction,
    onAuctionEnd: (event, prebid) => prebidCoordinator?.auctionEnded(event, prebid),
    prebid: composition.adapters.prebid,
    refresh: Object.freeze({
      configure: (config: unknown): void => {
        prebidRefreshConfig = composedPrebidRefreshConfig(config);
      },
      install: gptRuntime.installRefreshPolicy,
      policy: prebidRefreshPolicy,
    }),
    start: startPrebid,
  });
  const getBindings: NonNullable<RuntimeOptions['getBindings']> = (id) => {
    const provided = providedBindings?.(id);
    let config: unknown;
    if (provided !== undefined) {
      const descriptor = Object.getOwnPropertyDescriptor(provided, 'config');
      if (!descriptor || !('value' in descriptor)) return provided;
      config = descriptor.value;
    }
    if (id === 'creative' && config === undefined) config = creativeBoot;
    if (id === 'gpt_diagnostics' && config === undefined) config = diagnosticsBoot?.gpt;
    if (id === 'didomi' && config === undefined) config = acceptedBrowserBoot?.didomi;
    if (id === 'sourcepoint' && config === undefined) config = acceptedBrowserBoot?.sourcepoint;
    const interfaces = runtimeSession?.interfaces;
    if (!interfaces) throw new Error(`Integration interfaces are unavailable for ${id}`);
    return Object.freeze({
      config,
      interfaces,
    });
  };
  let preparedBrowserServices: PreparedBrowserServices | undefined;
  let auctionContextRegistry: AuctionContextRegistry | undefined;
  const dataDomeRuntime = createDataDomeRuntime();
  const didomiRuntime = createDidomiRuntime();
  const googleTagManagerRuntime = createGoogleTagManagerRuntime();
  const lockrRuntime = createLockrRuntime();
  const osanoRuntime = createOsanoRuntime();
  const permutiveRuntime = createPermutiveRuntime({
    registerContext: (contributor) => {
      const registry = auctionContextRegistry;
      const owner = runtimeSession;
      return registry && owner
        ? registerScopedContextContributor(registry, owner, 'permutive', contributor)
        : undefined;
    },
  });
  const sourcepointRuntime = createSourcepointRuntime();
  const testlightRuntime = createTestlightRuntime({
    enqueue: (callback) => {
      const queue = (runtimeOptions.target as { readonly que?: unknown }).que;
      if (!Array.isArray(queue) || typeof queue.push !== 'function') {
        throw new Error('Testlight TSJS queue is unavailable');
      }
      queue.push(callback);
    },
    started: () => log.info('Testlight integration initialized'),
    target: window as typeof window & { testlight?: { que?: unknown[] } },
  });
  let auctionBatchService: AuctionBatchService | undefined;
  let projectionParser: ((candidate: unknown) => object | undefined) | undefined;
  const frozenSlotResult = (result: Record<string, unknown>): Readonly<Record<string, unknown>> =>
    Object.freeze(result);
  const combineRequestResults = (
    requestedSlots: readonly string[],
    records: readonly (SlotRecord | undefined)[],
    validResults: readonly Readonly<Record<string, unknown>>[]
  ): Readonly<{ slots: readonly Readonly<Record<string, unknown>>[] }> => {
    let validIndex = 0;
    return Object.freeze({
      slots: Object.freeze(
        requestedSlots.map((slot, index) => {
          if (!records[index]) {
            return frozenSlotResult({
              slot,
              path: 'primary',
              outcome: 'failed',
              reason: 'slot_unresolved',
            });
          }
          const result = validResults[validIndex];
          validIndex += 1;
          return (
            result ??
            frozenSlotResult({
              slot,
              path: 'primary',
              outcome: 'failed',
              reason: 'internal_error',
            })
          );
        })
      ),
    });
  };
  const registrationError = (reason: SlotRegistrationFailure): AdUnitRegistrationError => {
    switch (reason) {
      case 'invalid_slot_id':
        return new AdUnitRegistrationError('invalid_code');
      case 'registry_capacity':
        return new AdUnitRegistrationError('registry_capacity');
      case 'duplicate_slot':
      case 'slot_quarantined':
      case 'stale_owner':
        return new AdUnitRegistrationError('slot_collision');
    }
  };
  const addProgrammaticAdUnits = (candidate: unknown): unknown => {
    const navigation = runtimeSession?.currentNavigation;
    const slots = browserServices?.slots;
    if (!navigation || !slots) throw new AdUnitRegistrationError('slot_collision');
    let snapshot: readonly SlotRecord[] | undefined;
    try {
      snapshot = slots.snapshotRegisteredSlots(navigation);
    } catch {
      throw new AdUnitRegistrationError('slot_collision');
    }
    if (!snapshot) throw new AdUnitRegistrationError('slot_collision');
    const knownSlots = new Set(snapshot.map(({ registeredSlotId }) => registeredSlotId));
    const prepared = prepareProgrammaticAdUnits(candidate, knownSlots);
    let registered: ReturnType<SlotService['register']>;
    try {
      registered = slots.register(
        navigation,
        prepared.map((unit) => ({
          directAuctionUnit: unit,
          registeredSlotId: unit.code,
          source: 'programmatic' as const,
        }))
      );
    } catch {
      throw new AdUnitRegistrationError('slot_collision');
    }
    if (!registered.ok) throw registrationError(registered.reason);
    return addAdUnitsResult(prepared);
  };
  const requestDirectAds = (candidate?: unknown): Promise<unknown> => {
    let validated: ReturnType<typeof validateRequestAdsOptions>;
    try {
      validated = validateRequestAdsOptions(candidate);
    } catch (error) {
      return Promise.reject(error);
    }
    const navigation = runtimeSession?.currentNavigation;
    const slots = browserServices?.slots;
    const snapshot = navigation && slots?.snapshotRegisteredSlots(navigation);
    if (!navigation || !slots || !snapshot) {
      const requested = validated.slots ?? Object.freeze([]);
      return Promise.resolve(
        Object.freeze({
          slots: Object.freeze(
            requested.map((slot) =>
              frozenSlotResult({
                slot,
                path: 'primary',
                outcome: 'cancelled',
                reason: 'navigation_disposed',
              })
            )
          ),
        })
      );
    }

    const recordsById = new Map(snapshot.map((record) => [record.registeredSlotId, record]));
    const requestedSlots = Object.freeze(
      validated.slots
        ? Array.from(validated.slots)
        : snapshot.map(({ registeredSlotId }) => registeredSlotId)
    );
    const selectedRecords = Object.freeze(requestedSlots.map((slot) => recordsById.get(slot)));
    const validRecords = selectedRecords.filter(
      (record): record is SlotRecord => record !== undefined
    );
    if (validRecords.length === 0) {
      return Promise.resolve(combineRequestResults(requestedSlots, selectedRecords, []));
    }

    const context = auctionContextRegistry?.snapshot() ?? Object.freeze({});
    const adUnits = validRecords.map((record) =>
      record.directAuctionUnit
        ? record.directAuctionUnit
        : Object.freeze({
            code: record.registeredSlotId,
            mediaTypes: Object.freeze({}),
            bids: Object.freeze([]),
          })
    );
    let requestBody: string;
    try {
      const serialized = serializeAuctionRequestBody(adUnits, context);
      if (!serialized) throw new Error('auction request body exceeds limit');
      requestBody = serialized;
    } catch {
      return Promise.resolve(
        combineRequestResults(
          requestedSlots,
          selectedRecords,
          validRecords.map((record) =>
            frozenSlotResult({
              slot: record.registeredSlotId,
              path: 'primary',
              outcome: 'failed',
              reason: 'internal_error',
            })
          )
        )
      );
    }
    const batches = auctionBatchService;
    if (!batches) {
      return Promise.resolve(
        combineRequestResults(
          requestedSlots,
          selectedRecords,
          validRecords.map((record) =>
            frozenSlotResult({
              slot: record.registeredSlotId,
              path: 'primary',
              outcome: 'cancelled',
              reason: 'navigation_disposed',
            })
          )
        )
      );
    }
    const batch = batches.create({
      navigation,
      requestBody,
      ...(validated.signal ? { signal: validated.signal } : {}),
      slots: Object.freeze(validRecords.map(({ registeredSlotId }) => registeredSlotId)),
      timeoutMs: validated.timeoutMs,
    });
    return batch.result.then((result) =>
      combineRequestResults(requestedSlots, selectedRecords, result.slots)
    );
  };
  const runtime = createRuntime({
    ...runtimeOptions,
    getBindings,
    getDiagnosticsForPublish: diagnosticsForPublish,
    kernel: {
      addAdUnits: addProgrammaticAdUnits,
      diagnostics: runtimeOptions.kernel.diagnostics,
      requestAds: requestDirectAds,
    },
    prepareOwner: (context) => {
      const boot = context.boot as unknown as AcceptedBrowserBoot;
      acceptedBrowserBoot = boot;
      creativeBoot = boot.creative;
      diagnosticsBoot = boot.diagnostics;
      const cachePolicy =
        boot.cachePolicy === undefined ? undefined : parseCacheFetchPolicyV1(boot.cachePolicy);
      const parseProjection = (candidate: unknown): object | undefined =>
        parseBrowserAuctionProjectionV1(candidate, boot.cachePolicy);
      const initialProjection = prepareInitialAuctionProjection(
        boot.auctionProjection,
        parseProjection
      );
      if (!initialProjection) throw new Error('Accepted boot projection is unavailable');
      const preparedRenderTrace = createRenderTrace({
        ...(typeof document === 'undefined' ? {} : { document }),
        onSubscriberError: (error) => log.warn('render diagnostics: subscriber failed', error),
        overlayEnabled: boot.diagnostics.renderTraceOverlay,
      });
      const preparedDiagnosticsBus = createDiagnosticsBus({
        manifest: boot.manifest,
        onObservation: consumeCoreObservation,
        onSubscriberError: (error) => log.warn('diagnostics bus: subscriber failed', error),
      });
      renderTrace = preparedRenderTrace;
      diagnosticsBus = preparedDiagnosticsBus;
      const preparedGptDiagnosticsFacts = boot.diagnostics.gpt.active
        ? createGptDiagnosticsFactBuffer({
            onConsumerError: (error) => log.warn('gpt diagnostics: fact consumer failed', error),
          })
        : undefined;
      const preparedGptDiagnosticsRuntime = preparedGptDiagnosticsFacts
        ? createGptDiagnosticsRuntime(preparedGptDiagnosticsFacts)
        : undefined;
      gptDiagnosticsFacts = preparedGptDiagnosticsFacts;
      gptDiagnosticsRuntime = preparedGptDiagnosticsRuntime;
      context.onDispose(() => {
        preparedGptDiagnosticsFacts?.dispose();
        preparedDiagnosticsBus.dispose();
        preparedRenderTrace.dispose();
        if (gptDiagnosticsFacts === preparedGptDiagnosticsFacts) {
          gptDiagnosticsFacts = undefined;
        }
        if (gptDiagnosticsRuntime === preparedGptDiagnosticsRuntime) {
          gptDiagnosticsRuntime = undefined;
        }
        if (diagnosticsBus === preparedDiagnosticsBus) diagnosticsBus = undefined;
        if (renderTrace === preparedRenderTrace) renderTrace = undefined;
      });
      const reconciliation =
        typeof document === 'undefined' || typeof MutationObserver === 'undefined'
          ? undefined
          : createBrowserSlotReconciliationBoundary(document, MutationObserver);
      const artifacts = createCommittedArtifactStore();
      const slotService = createSlotService({
        disposeCommittedArtifact: (navigationGeneration, registeredSlotId) => {
          const artifact = artifacts.current(registeredSlotId);
          if (artifact?.navigationGeneration === navigationGeneration) {
            artifacts.release(artifact);
          }
        },
        googletag: composition.adapters.googletag,
        ...(reconciliation ? { reconciliation } : {}),
        warnPublisherHandoffMismatch: (message, details) => log.warn(message, details),
      });
      const targetingService = createTargetingService();
      const reservationService = createReservationService({
        prepareRenderSource: (candidate) => parseBidRenderSourceV1(candidate, cachePolicy),
      });
      const rendererNonces = createRendererNonceRegistry();
      const publisherOrigin = window.location.origin;
      const fetchCache = globalThis.fetch;
      const rendererUrl = new URL(APS_RENDERER_V1_PATH, publisherOrigin).href;
      const renderDirectAdm = Object.freeze(
        (attempt: RenderAttempt, container: HTMLElement): boolean => {
          try {
            return renderDirectAdmAttempt({
              attempt,
              container,
              prepareIframe: prepareAdmIframe,
              publisherOrigin,
            });
          } catch {
            return false;
          }
        }
      );
      const renderDirectCache = Object.freeze(
        (attempt: RenderAttempt, container: HTMLElement): boolean => {
          if (!cachePolicy) {
            try {
              attempt.fail('descriptor_invalid');
            } catch {
              // The admitted attempt remains the only terminal authority.
            }
            return false;
          }
          if (typeof fetchCache !== 'function') {
            try {
              attempt.fail('cache_network_error');
            } catch {
              // The admitted attempt remains the only terminal authority.
            }
            return false;
          }
          try {
            return renderDirectCacheAttempt({
              attempt,
              cachePolicy,
              container,
              fetcher: (input, init) => fetchCache(input, init),
              prepareIframe: prepareAdmIframe,
              publisherOrigin,
            });
          } catch {
            return false;
          }
        }
      );
      const renderDirectAps = Object.freeze(
        (attempt: RenderAttempt, container: HTMLElement): boolean => {
          try {
            return renderDirectApsAttempt({
              attempt,
              container,
              messaging: composition.adapters.messaging,
              nonces: rendererNonces,
              publisherOrigin,
            });
          } catch {
            return false;
          }
        }
      );
      const resolveCacheAdm: NonNullable<PucBridgeOptions['resolveCacheAdm']> = (
        attempt,
        onResolved
      ): boolean => {
        if (!cachePolicy) {
          try {
            attempt.fail('descriptor_invalid');
          } catch {
            // The admitted attempt remains the only terminal authority.
          }
          return false;
        }
        if (typeof fetchCache !== 'function') {
          try {
            attempt.fail('cache_network_error');
          } catch {
            // The admitted attempt remains the only terminal authority.
          }
          return false;
        }
        try {
          return resolveCacheAdmAttempt({
            attempt: attempt as RenderAttempt,
            cachePolicy,
            fetcher: (input, init) => fetchCache(input, init),
            onResolved,
          });
        } catch {
          return false;
        }
      };
      const resolveDirectContainer = (record: SlotRecord): HTMLElement | undefined => {
        try {
          if (typeof document === 'undefined') return undefined;
          const identifiers =
            record.source === 'programmatic'
              ? new Set([record.registeredSlotId])
              : new Set(record.domAliases);
          if (identifiers.size === 0) return undefined;
          const matches = new Set<HTMLElement>();
          const elements = document.querySelectorAll('[id]');
          for (let index = 0; index < elements.length; index += 1) {
            const element = elements.item(index);
            if (element instanceof HTMLElement && identifiers.has(element.id)) {
              matches.add(element);
            }
          }
          return matches.size === 1 ? Array.from(matches)[0] : undefined;
        } catch {
          return undefined;
        }
      };
      const fetchAuction = compositionOptions.auctionFetcherForTest ?? globalThis.fetch;
      const createOwnedAttempt = (owner: RenderAttemptScope) =>
        createRenderAttempt({
          artifacts,
          owner,
          prepareRenderSource: (candidate) => {
            const source = parseBidRenderSourceV1(candidate, cachePolicy);
            return source ? Object.freeze(source) : undefined;
          },
          publishDiagnostics: preparedDiagnosticsBus.publish,
          reservations: reservationService,
        });
      const batchCoordinator = createAuctionBatchService({
        ...(cachePolicy ? { cachePolicy } : {}),
        createAttempt: createOwnedAttempt,
        fetcher: (input, init) => {
          if (typeof fetchAuction !== 'function') return Promise.reject(new Error('unavailable'));
          return fetchAuction(input, init);
        },
        parseResponse: parseTrustedServerAuctionResponseV1,
        renderWinner: (attempt) => {
          const record = slotService.resolveRegisteredSlot(attempt.slot);
          const container = record && resolveDirectContainer(record);
          if (!container) {
            attempt.fail('slot_unresolved');
            return false;
          }
          if (attempt.renderSource?.type === 'aps') return renderDirectAps(attempt, container);
          if (attempt.renderSource?.type === 'adm') return renderDirectAdm(attempt, container);
          if (attempt.renderSource?.type === 'cache') return renderDirectCache(attempt, container);
          attempt.fail('winner_not_renderable');
          return false;
        },
      });
      const services = Object.freeze({
        artifacts,
        auctionBatches: batchCoordinator,
        reservations: reservationService,
        rendererNonces,
        renderDirectAdm,
        renderDirectAps,
        renderDirectCache,
        slots: slotService,
        targeting: targetingService,
      });
      preparedBrowserServices = Object.freeze({
        createAttempt: createOwnedAttempt,
        publisherOrigin,
        rendererUrl,
        resolveCacheAdm,
        services,
      });
      const session = createRuntimeSession({
        createIdentityIssuer:
          compositionOptions.createIdentityIssuerForTest ?? createBrowserNavigationIdentityIssuer,
        interfaces: Object.freeze({
          adapters: composition.adapters,
          creative: creativeRuntime,
          datadome: dataDomeRuntime,
          diagnostics: Object.freeze({ subscribe: preparedDiagnosticsBus.subscribe }),
          didomi: didomiRuntime,
          google_tag_manager: googleTagManagerRuntime,
          ...(preparedGptDiagnosticsRuntime
            ? { gpt_diagnostics: preparedGptDiagnosticsRuntime }
            : {}),
          gpt: gptIntegrationRuntime,
          lockr: lockrRuntime,
          osano: osanoRuntime,
          permutive: permutiveRuntime,
          prebid: prebidRuntime,
          sourcepoint: sourcepointRuntime,
          testlight: testlightRuntime,
          ...services,
        }),
        onNavigationDispose: (navigationGeneration) => {
          artifacts.disposeNavigation(navigationGeneration);
          for (const registeredSlotId of renderTraceSlotsByNavigation.get(navigationGeneration) ??
            []) {
            preparedRenderTrace.prune(registeredSlotId);
          }
          renderTraceSlotsByNavigation.delete(navigationGeneration);
        },
      });
      context.onDispose(() => {
        batchCoordinator.dispose();
        session.dispose();
        artifacts.dispose();
        reservationService.dispose();
        rendererNonces.dispose();
        slotService.dispose();
        targetingService.dispose();
        composition.adapters.googletag.dispose();
        composition.adapters.prebid.dispose();
        if (runtimeSession === session) {
          runtimeSession = undefined;
          preparedBrowserServices = undefined;
          browserServices = undefined;
          auctionBatchService = undefined;
          auctionContextRegistry = undefined;
          projectionParser = undefined;
          acceptedBrowserBoot = undefined;
          creativeBoot = undefined;
          diagnosticsBoot = undefined;
          renderTraceSlotsByNavigation.clear();
        }
      });
      const navigation = session.startInitialNavigation(initialProjection);
      if (!navigation.ok) throw new Error(navigation.reason);

      const initialRegistrations = [
        ...projectionSlots(initialProjection).map((registeredSlotId) => ({
          registeredSlotId,
          source: 'server' as const,
        })),
        ...(compositionOptions.admittedProgrammaticSlotsForTest ?? []).map((registeredSlotId) => ({
          registeredSlotId,
          source: 'programmatic' as const,
        })),
      ];
      if (!slotService.register(navigation.value, initialRegistrations).ok) {
        throw new Error('Initial slots exceed the shared registry');
      }
      const contextRegistry = createAuctionContextRegistry({
        manifestIntegrationIds: Object.freeze(boot.manifest.integrations.map(({ id }) => id)),
        onContributorFailure: (failure) => log.warn('auction context: contributor failed', failure),
        runtimeOwner: session,
      });
      runtimeSession = session;
      auctionBatchService = batchCoordinator;
      auctionContextRegistry = contextRegistry;
      projectionParser = parseProjection;
      return runtimeOptions.prepareOwner?.(context);
    },
    activateCore: (context) => {
      const prepared = preparedBrowserServices;
      if (!prepared) throw new Error('Browser services are unavailable');
      const facts = gptDiagnosticsFacts;
      if (facts) {
        const releaseCapture = activateGptDiagnosticsFactCapture(
          composition.adapters.googletag,
          facts
        );
        if (!releaseCapture) throw new Error('GPT diagnostics capture is unavailable');
        context.onDispose(releaseCapture);
      }
      const pucBridge = createPucBridge({
        messaging: composition.adapters.messaging,
        publisherOrigin: prepared.publisherOrigin,
        ...(compositionOptions.pucSchedulerForTest
          ? { scheduler: compositionOptions.pucSchedulerForTest }
          : {}),
        rendererNonces: prepared.services.rendererNonces,
        rendererUrl: prepared.rendererUrl,
        reservations: prepared.services.reservations,
        resizeCollapsedShell: resizeCollapsedPucShell,
        resolveCacheAdm: prepared.resolveCacheAdm,
      });
      context.onDispose(() => pucBridge.dispose());
      browserServices = Object.freeze({ ...prepared.services, pucBridge });
      const coordinator = createPrebidSelectionCoordinator({
        activateAttempt: ({ attempt, owner, preparedBid }): boolean => {
          const artifact = Object.freeze({
            kind: 'puc' as const,
            attemptId: attempt.id,
            slot: attempt.slot,
            navigationGeneration: attempt.navigationGeneration,
            dispose: () => undefined,
          });
          const input = Object.freeze({
            artifact,
            attempt,
            owner,
            reservationId: preparedBid.bid.adId,
          });
          return pucBridge.registerGamAttempt(input);
        },
        createAttempt: prepared.createAttempt,
        reservations: prepared.services.reservations,
      });
      prebidCoordinator = coordinator;
      context.onDispose(() => {
        coordinator.dispose();
        if (prebidCoordinator === coordinator) prebidCoordinator = undefined;
      });
      browserServices.slots.activate();
      browserServices.slots.start();
      compositionOptions.coreActivations.correctnessGptListeners(
        context,
        composition.adapters,
        browserServices
      );
      return runtimeOptions.activateCore?.(context);
    },
  });
  return Object.freeze({
    adapters: composition.adapters,
    runtime,
    runtimeSessionForTest: () => runtimeSession,
    pageBidsControllerForTest: (): PageBidsController | undefined => {
      const navigation = runtimeSession?.currentNavigation;
      if (!navigation || !browserServices || !projectionParser) return undefined;
      return createPageBidsController({
        navigation,
        parseProjection: projectionParser,
        slotRegistry: browserServices.slots.projectionRegistry(navigation),
      });
    },
    projectionSlotsForTest: () => browserServices?.slots.registeredSlotIdsForTest(),
    auctionContextRegistryForTest: () => auctionContextRegistry,
    slotServiceForTest: () => browserServices?.slots,
    targetingServiceForTest: () => browserServices?.targeting,
    reservationServiceForTest: () => browserServices?.reservations,
    rendererNonceRegistryForTest: () => browserServices?.rendererNonces,
    pucBridgeForTest: () => browserServices?.pucBridge,
    publishGptWinnerForTest: (
      input: Omit<
        GptWinnerPublicationInput,
        'googletag' | 'navigation' | 'pucBridge' | 'reservations' | 'slots' | 'targeting'
      >
    ): Promise<GptWinnerPublicationResult> => {
      const services = browserServices;
      const navigation = runtimeSession?.currentNavigation;
      if (!services || !navigation) {
        return Promise.resolve(Object.freeze({ ok: false, reason: 'gpt_request_failed' }));
      }
      return publishGptWinner({
        ...input,
        googletag: composition.adapters.googletag,
        navigation,
        pucBridge: services.pucBridge,
        reservations: services.reservations,
        slots: services.slots,
        targeting: services.targeting,
      });
    },
    startGptSlotOperationForTest: (
      input: Omit<GptSlotOperationInput, 'pucBridge' | 'slots'>
    ): SlotOperationCreationResult => {
      const services = browserServices;
      if (!services) return Object.freeze({ ok: false, reason: 'invalid_attempt' });
      return startGptSlotOperation({
        ...input,
        pucBridge: services.pucBridge,
        slots: services.slots,
      });
    },
  });
}
