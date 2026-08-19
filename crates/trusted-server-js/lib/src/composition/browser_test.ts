import {
  createBrowserGoogletagAdapter,
  createNoopGoogletagAdapter,
  type GoogletagAdapter,
  type GoogletagDiagnosticsFact,
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
import { parseTrustedServerAuctionResponseV1 } from '../core/auction';
import { snapshotTsjsBootV1 } from '../core/contracts/boot';
import type {
  BootManifestV1,
  BrowserAuctionProjectionV1,
  BrowserAuctionSlotV1,
  CreativeBootV1,
  DiagnosticsBootV1,
} from '../core/types';
import {
  createRenderTraceStore,
  type RenderTraceGptFactV1,
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
import {
  APS_RENDERER_V1_PATH,
  renderDirectApsAttempt,
  renderPucApsAttempt,
} from '../integrations/aps/render';
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
  projectGptTraceFact,
  type GptDiagnosticsFactBuffer,
} from '../integrations/gpt/diagnostics_facts';
import {
  createPrebidSelectionCoordinator,
  publishPrebidBid,
  type PrebidSelectionCoordinator,
} from '../integrations/prebid/module';
import {
  createPrebidRefreshPolicy,
  createPrebidSyntheticRefreshRunner,
  preparePrebidRegisteredRefreshAuction,
} from '../integrations/prebid/refresh';
import { createPrebidStartup } from '../integrations/prebid/startup';
import { createLockrRuntime } from '../integrations/lockr/module';
import { createOsanoRuntime } from '../integrations/osano/module';
import { createPermutiveRuntime } from '../integrations/permutive/module';
import { createSourcepointRuntime } from '../integrations/sourcepoint/module';
import { createTestlightRuntime } from '../integrations/testlight/module';
import {
  createBrowserNavigationIdentityIssuer,
  mintBrowserLifecycleTicket,
} from '../kernel/identity';
import {
  createDiagnosticsIngress,
  type DiagnosticsIngress,
  type DiagnosticsObservation,
} from '../kernel/diagnostics';
import type {
  NavigationIdentityIssuerFactory,
  RenderAttemptScope,
  RuntimeSession,
} from '../kernel/sessions';
import { createRuntimeSession } from '../kernel/sessions';
import { trustedDocumentHttpOrigin } from '../shared/origin';
import type {
  CoreActivationContext,
  IntegrationCatalogEntry,
  IntegrationRegistration,
} from '../kernel/integration_registry';
import { RELEASE_CATALOG } from '../kernel/release_catalog';
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
  createBootstrapNonceRegistry,
  createRenderAttempt,
  createRendererNonceRegistry,
  createSlotOperation,
  renderDirectAdmAttempt,
  type RenderAttempt,
  type CommittedArtifactStore,
  type BootstrapNonceRegistry,
  type RendererNonceRegistry,
  type SlotOperationCreationResult,
} from '../services/render';
import { resizeCollapsedPucShell } from '../core/puc_shell';
import { createPucBridge, type PucBridge, type PucBridgeOptions } from '../services/puc_bridge';
import {
  createBrowserSlotReconciliationBoundary,
  createSlotService,
  type SlotRecord,
  type SlotRegistrationFailure,
  type SlotService,
} from '../services/slots';
import { createTargetingService, type TargetingService } from '../services/targeting';

function isEffectivelyVisible(element: Element | null): boolean {
  try {
    if (!element || !(element instanceof HTMLElement) || !element.isConnected) return false;
    const rectangle = element.getBoundingClientRect();
    if (rectangle.width <= 0 || rectangle.height <= 0) return false;
    let current: HTMLElement | null = element;
    while (current) {
      const style = getComputedStyle(current);
      if (
        style.display === 'none' ||
        style.visibility === 'hidden' ||
        Number.parseFloat(style.opacity || '1') === 0
      ) {
        return false;
      }
      current = current.parentElement;
    }
    return true;
  } catch {
    return false;
  }
}

export interface BrowserAdapters {
  readonly googletag: GoogletagAdapter;
  readonly messaging: MessagingAdapter;
  readonly prebid: PrebidAdapter;
}

export interface BrowserComposition {
  readonly adapters: Readonly<BrowserAdapters>;
}

export const BROWSER_TEST_DIAGNOSTICS_PROVIDER_ID = 'browser_test_diagnostics_provider';
export const BROWSER_TEST_TRACE_PROVIDER_ID = 'browser_test_trace_provider';
const TRUSTED_BROWSER_TEST_RUNTIME_SRC = `/static/tsjs=tsjs-unified.min.js?v=${'c'.repeat(64)}`;

function installBrowserTestRuntimeScript(runtimeDocument: Document): void {
  if (runtimeDocument.currentScript) return;
  const script = runtimeDocument.createElement('script');
  script.id = 'trustedserver-js';
  script.src = new URL(TRUSTED_BROWSER_TEST_RUNTIME_SRC, runtimeDocument.location.origin).href;
  runtimeDocument.head.insertBefore(script, null);
  Object.defineProperty(runtimeDocument, 'currentScript', {
    configurable: true,
    value: script,
  });
}

export interface BrowserServices {
  readonly artifacts: CommittedArtifactStore;
  readonly auctionBatches: AuctionBatchService;
  readonly bootstrapNonces: BootstrapNonceRegistry;
  readonly pucBridge: PucBridge;
  readonly reservations: ReservationService;
  readonly rendererNonces: RendererNonceRegistry;
  readonly renderDirectAdm: (attempt: RenderAttempt, container: HTMLElement) => boolean;
  readonly renderDirectAps: (attempt: RenderAttempt, container: HTMLElement) => boolean;
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
  /** Build the explicit test-only provider for the diagnostics capability chain. */
  readonly createDiagnosticsCapabilityProviderRegistrationForTest: () => IntegrationRegistration;
  /** Build the explicit test-only provider for an overlay-only trace capability chain. */
  readonly createTraceCapabilityProviderRegistrationForTest: () => IntegrationRegistration;
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
  /** Join one candidate GPT attempt through the runtime-owned services in tests. */
  readonly startGptSlotOperationForTest: (
    input: Omit<GptSlotOperationInput, 'createSlotOperation' | 'pucBridge' | 'slots'>
  ) => SlotOperationCreationResult;
  /** Publish one candidate server winner through the ordered GPT transaction in tests. */
  readonly publishGptWinnerForTest: (
    input: Omit<
      GptWinnerPublicationInput,
      | 'createSlotOperation'
      | 'googletag'
      | 'navigation'
      | 'pucBridge'
      | 'reservations'
      | 'slots'
      | 'targeting'
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
  readonly coreActivations?: BrowserCoreActivations;
  readonly creativeActivationForTest?: (config: Readonly<CreativeBootV1>) => () => void;
  readonly creativeStartupForTest?: (config: Readonly<CreativeBootV1>) => void;
  readonly createIdentityIssuerForTest?: NavigationIdentityIssuerFactory;
  readonly admittedProgrammaticSlotsForTest?: readonly string[];
  readonly gptStartupForTest?: (config: unknown) => void;
  readonly pageBidsFetcherForTest?: PageBidsFetcher;
  readonly prebidStartupForTest?: (config: unknown) => void;
  readonly pucSchedulerForTest?: PucBridgeOptions['scheduler'];
}

const TEST_CONFIG_ORDER = Object.freeze([
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

function testConfigProduct(id: string): string | undefined {
  if (id === 'gpt' || id === 'gpt_later') return 'gpt';
  if (id === 'osano_consent' || id === 'osano_lifecycle') return 'osano';
  if (id === 'permutive_context' || id === 'permutive_lifecycle') return 'permutive';
  if (id === 'prebid' || id === 'prebid_later') return 'prebid';
  if (id === 'sourcepoint_consent' || id === 'sourcepoint_lifecycle') return 'sourcepoint';
  return TEST_CONFIG_ORDER.includes(id) ? id : undefined;
}

function defaultBrowserTestConfig(id: string): Readonly<Record<string, unknown>> {
  if (id === 'didomi') return { proxyPath: '/integrations/didomi/consent/' };
  if (id === 'gpt') return { gamAttributionEnabled: false };
  if (id === 'prebid') {
    return {
      accountId: 'test',
      timeout: 1_000,
      debug: false,
      bidders: [],
      clientSideBidders: [],
      excludedGamAdUnitPathSuffixes: [],
    };
  }
  if (id === 'sourcepoint') return { rewriteSdk: true };
  return {};
}

function testIntegrationConfigs(manifest: unknown): Readonly<Record<string, unknown>> {
  const integrations =
    typeof manifest === 'object' &&
    manifest !== null &&
    Array.isArray((manifest as { integrations?: unknown }).integrations)
      ? (manifest as { integrations: Array<{ id?: unknown }> }).integrations
      : [];
  const selected = new Set(
    integrations.flatMap(({ id }) => (typeof id === 'string' ? [testConfigProduct(id)] : []))
  );
  return {
    version: 1,
    entries: TEST_CONFIG_ORDER.filter((id) => selected.has(id)).map((id) => ({
      id,
      config: defaultBrowserTestConfig(id),
    })),
  };
}

function capturedBrowserTestRuntimeOptions(options: RuntimeOptions): RuntimeOptions {
  try {
    if (
      typeof options.boot !== 'object' ||
      options.boot === null ||
      Array.isArray(options.boot) ||
      Object.getPrototypeOf(options.boot) !== Object.prototype
    ) {
      return options;
    }
    const fields = options.boot as Readonly<Record<string, unknown>>;
    const candidate = Object.prototype.hasOwnProperty.call(fields, 'abi')
      ? fields
      : {
          abi: 1,
          releaseId: options.releaseId,
          manifest: options.manifest,
          auctionProjection: fields['auctionProjection'],
          integrations: Object.prototype.hasOwnProperty.call(fields, 'integrations')
            ? fields['integrations']
            : testIntegrationConfigs(options.manifest),
          creative: fields['creative'],
          diagnostics: fields['diagnostics'],
        };
    const boot = snapshotTsjsBootV1(candidate, options.releaseId);
    if (!boot) return options;
    const catalog = options.catalog?.map((entry): IntegrationCatalogEntry => {
      const canonical = RELEASE_CATALOG.find(({ id }) => id === entry.id);
      return Object.freeze({
        ...entry,
        config: canonical?.config ?? entry.config ?? null,
      });
    });
    return {
      ...options,
      boot,
      manifest: boot.manifest,
      ...(catalog === undefined ? {} : { catalog: Object.freeze(catalog) }),
    };
  } catch {
    return options;
  }
}

interface AcceptedBrowserBoot {
  readonly auctionProjection: object;
  readonly creative: Readonly<CreativeBootV1>;
  readonly diagnostics: Readonly<DiagnosticsBootV1>;
  readonly manifest: Readonly<BootManifestV1>;
}

interface PreparedBrowserServices {
  readonly createAttempt: (
    owner: RenderAttemptScope,
    parentAttemptId?: string
  ) => ReturnType<typeof createRenderAttempt>;
  readonly publisherOrigin: string;
  readonly renderProjectedFallback: (attempt: RenderAttempt) => boolean;
  readonly rendererUrl: string;
  readonly services: Readonly<Omit<BrowserServices, 'pucBridge'>>;
}

interface PageBidsResponse {
  readonly ok: boolean;
  readonly json: () => Promise<unknown>;
}

type PageBidsFetcher = (
  input: string,
  init: Readonly<{
    credentials: 'include';
    headers: Readonly<{ 'X-TSJS-Page-Bids': '1' }>;
    signal: AbortSignal;
  }>
) => PromiseLike<PageBidsResponse>;

interface PageBidsNavigationLifecycle {
  readonly activate: () => () => void;
  readonly start: () => void;
}

type GptProjectionPublisher = (
  navigation: NonNullable<RuntimeSession['currentNavigation']>,
  projection: Readonly<BrowserAuctionProjectionV1>,
  requestClass: string
) => void;

const noopGptProjectionPublisher: GptProjectionPublisher = () => undefined;

function resolveProjectedSlotElement(
  placement: Readonly<BrowserAuctionSlotV1>
): HTMLElement | undefined {
  try {
    if (typeof document === 'undefined') return undefined;
    const exact = document.getElementById(placement.divId);
    if (exact instanceof HTMLElement) return exact;
    const prefixMatches = [...document.querySelectorAll<HTMLElement>('[id]')].filter(
      (element) => element.id.startsWith(placement.divId) && !element.id.endsWith('-container')
    );
    if (prefixMatches.length === 1) return prefixMatches[0];
    const visible = prefixMatches.filter((element) => isEffectivelyVisible(element));
    if (visible.length === 1) return visible[0];
    const active = visible.filter((element) => {
      const bounds = element.getBoundingClientRect();
      return bounds.width > 0 && bounds.height > 0;
    });
    return active.length === 1 ? active[0] : undefined;
  } catch {
    return undefined;
  }
}

function currentBrowserPath(): string | undefined {
  try {
    return `${window.location.pathname}${window.location.search}`;
  } catch {
    return undefined;
  }
}

function restoreHistoryMethod(
  name: 'pushState' | 'replaceState',
  previous: PropertyDescriptor | undefined,
  installed: History['pushState']
): void {
  try {
    const current = Object.getOwnPropertyDescriptor(window.history, name);
    if (!current || !('value' in current) || current.value !== installed) return;
    if (previous) Object.defineProperty(window.history, name, previous);
    else Reflect.deleteProperty(window.history, name);
  } catch {
    // A publisher replacement remains authoritative; the disposed wrapper is inert.
  }
}

/** Own the canonical page-bids fetch and one replacement session per SPA navigation. */
function createPageBidsNavigationLifecycle(options: {
  readonly fetcher?: PageBidsFetcher;
  readonly onProjectionCommitted?: (
    navigation: NonNullable<RuntimeSession['currentNavigation']>,
    projection: Readonly<object>
  ) => void;
  readonly runtimeSession: () => RuntimeSession | undefined;
  readonly services: () => Readonly<BrowserServices> | undefined;
  readonly projectionParser: () => ((candidate: unknown) => object | undefined) | undefined;
}): PageBidsNavigationLifecycle {
  let active = false;
  let disposed = false;
  let started = false;
  let appliedPath: string | undefined;
  let currentPath: string | undefined;
  let release: (() => void) | undefined;

  const rollBackPath = (
    path: string,
    navigation?: NonNullable<RuntimeSession['currentNavigation']>
  ): void => {
    if (currentPath !== path || (navigation && !navigation.isCurrent())) return;
    currentPath = appliedPath;
  };

  const requestProjection = async (path: string): Promise<void> => {
    const session = options.runtimeSession();
    const replacement = session?.replaceNavigation();
    if (!replacement?.ok) {
      rollBackPath(path);
      return;
    }
    const navigation = replacement.value;
    const services = options.services();
    const parseProjection = options.projectionParser();
    if (!services || !parseProjection) {
      rollBackPath(path, navigation);
      return;
    }
    const controller = createPageBidsController({
      navigation,
      parseProjection,
      slotRegistry: services.slots.projectionRegistry(navigation),
    });
    const fetcher = options.fetcher ?? globalThis.fetch;
    if (typeof fetcher !== 'function') {
      rollBackPath(path, navigation);
      return;
    }
    let committed = false;
    try {
      const response = await fetcher(`/_ts/page-bids?path=${encodeURIComponent(path)}`, {
        credentials: 'include',
        headers: { 'X-TSJS-Page-Bids': '1' },
        signal: navigation.signal,
      });
      if (!navigation.isCurrent()) return;
      if (!response.ok) {
        rollBackPath(path, navigation);
        return;
      }
      const candidate = await response.json();
      if (!navigation.isCurrent()) return;
      const result = controller.commit(candidate);
      if (result.status === 'committed') {
        committed = true;
        appliedPath = path;
        const projection = navigation.currentAuctionProjection;
        if (projection) options.onProjectionCommitted?.(navigation, projection);
      }
      if (result.status === 'rejected' && result.reason !== 'stale') {
        rollBackPath(path, navigation);
        log.warn('page-bids: rejected navigation projection', result.reason);
      }
    } catch (error) {
      if (!navigation.signal.aborted) {
        if (!committed) rollBackPath(path, navigation);
        log.warn('page-bids: projection request failed', error);
      }
    }
  };

  const navigateIfChanged = (): void => {
    if (!active || !started || disposed) return;
    const path = currentBrowserPath();
    if (path === undefined || path === currentPath) return;
    currentPath = path;
    void requestProjection(path);
  };

  return Object.freeze({
    activate: (): (() => void) => {
      if (active || disposed) throw new Error('Page-bids navigation owner is unavailable');
      const history = window.history;
      const previousPushState = Object.getOwnPropertyDescriptor(history, 'pushState');
      const previousReplaceState = Object.getOwnPropertyDescriptor(history, 'replaceState');
      const pushState = history.pushState;
      const replaceState = history.replaceState;
      const wrap = (original: History['pushState']): History['pushState'] =>
        function wrappedHistoryState(
          this: History,
          data: unknown,
          unused: string,
          url?: string | URL | null
        ): void {
          Reflect.apply(original, this, [data, unused, url]);
          navigateIfChanged();
        };
      const wrappedPushState = wrap(pushState);
      const wrappedReplaceState = wrap(replaceState);
      const onPopState = (): void => navigateIfChanged();
      try {
        Object.defineProperty(history, 'pushState', {
          configurable: true,
          enumerable: previousPushState?.enumerable ?? false,
          value: wrappedPushState,
          writable: true,
        });
        Object.defineProperty(history, 'replaceState', {
          configurable: true,
          enumerable: previousReplaceState?.enumerable ?? false,
          value: wrappedReplaceState,
          writable: true,
        });
        window.addEventListener('popstate', onPopState);
        active = true;
      } catch (error) {
        restoreHistoryMethod('replaceState', previousReplaceState, wrappedReplaceState);
        restoreHistoryMethod('pushState', previousPushState, wrappedPushState);
        throw error;
      }
      let released = false;
      release = (): void => {
        if (released) return;
        released = true;
        disposed = true;
        active = false;
        window.removeEventListener('popstate', onPopState);
        restoreHistoryMethod('replaceState', previousReplaceState, wrappedReplaceState);
        restoreHistoryMethod('pushState', previousPushState, wrappedPushState);
      };
      return release;
    },
    start: (): void => {
      if (!active || disposed) return;
      currentPath = currentBrowserPath();
      appliedPath = currentPath;
      started = true;
    },
  });
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
      ? createBrowserGoogletagAdapter(options.target, {
          reportDiagnosticsFailure: (code) =>
            log.warn('GPT diagnostics identity unavailable', code),
        })
      : createBrowserGoogletagAdapter(undefined, {
          reportDiagnosticsFailure: (code) =>
            log.warn('GPT diagnostics identity unavailable', code),
        }));
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
 * Construct the sole browser runtime composition without claiming a global.
 *
 * The core entry point owns the one production claim; tests may construct the
 * same composition against explicit targets and adapters.
 */
export function createTestBrowserRuntimeComposition(
  providedRuntimeOptions: RuntimeOptions,
  compositionOptions: TestBrowserRuntimeCompositionOptions
): BrowserRuntimeComposition {
  const runtimeOptions = capturedBrowserTestRuntimeOptions(providedRuntimeOptions);
  const runtimeDocument =
    runtimeOptions.document ?? (typeof document === 'undefined' ? undefined : document);
  if (runtimeDocument) installBrowserTestRuntimeScript(runtimeDocument);
  const composition = createBrowserComposition(compositionOptions);
  const providedBindings = runtimeOptions.getBindings;
  let browserServices: Readonly<BrowserServices> | undefined;
  let gptProjectionPublisher = noopGptProjectionPublisher;
  let projectionParser: ((candidate: unknown) => object | undefined) | undefined;
  let runtimeSession: RuntimeSession | undefined;
  let creativeBoot: Readonly<CreativeBootV1> | undefined;
  let diagnosticsBoot: Readonly<DiagnosticsBootV1> | undefined;
  let diagnosticsIngress: DiagnosticsIngress | undefined;
  let gptDiagnosticsFacts: GptDiagnosticsFactBuffer | undefined;
  let renderTrace: RenderTraceRuntimeOwner | undefined;
  const renderTraceSlotsByNavigation = new Map<object, Set<string>>();
  const consumeCoreObservation = (observation: DiagnosticsObservation): void => {
    if (
      observation['kind'] === 'slotRequested' ||
      observation['kind'] === 'slotResponseReceived' ||
      observation['kind'] === 'slotRenderEnded' ||
      observation['kind'] === 'slotOnload' ||
      observation['kind'] === 'impressionViewable' ||
      observation['kind'] === 'slotVisibilityChanged'
    ) {
      try {
        renderTrace?.observeGptFact(
          observation as unknown as Readonly<RenderTraceGptFactV1>,
          (elementId) => {
            if (typeof elementId !== 'string' || elementId === '') return undefined;
            const slots = browserServices?.slots;
            const slot =
              slots?.resolveDomAlias(elementId) ?? slots?.resolveRegisteredSlot(elementId);
            if (!slot?.traceToken) return undefined;
            let element: HTMLElement | undefined;
            if (typeof document !== 'undefined') {
              const matches = [...document.querySelectorAll<HTMLElement>('[id]')].filter(
                (candidate) => candidate.id === elementId
              );
              if (matches.length === 1) element = matches[0];
            }
            return Object.freeze({
              slotId: slot.registeredSlotId,
              navigationGeneration: slot.navigationGeneration,
              traceToken: slot.traceToken,
              ...(element === undefined
                ? {}
                : { elementId: element.id, visible: isEffectivelyVisible(element) }),
            });
          }
        );
      } catch {
        // Render tracing never affects an already-committed adapter observation.
      }
      return;
    }
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
    return Object.freeze({ renderTrace: trace.diagnostics });
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
  const pageBidsNavigation = createPageBidsNavigationLifecycle({
    ...(compositionOptions.pageBidsFetcherForTest
      ? { fetcher: compositionOptions.pageBidsFetcherForTest }
      : {}),
    onProjectionCommitted: (navigation, projection) =>
      gptProjectionPublisher(
        navigation,
        projection as Readonly<BrowserAuctionProjectionV1>,
        'page-bids'
      ),
    projectionParser: () => projectionParser,
    runtimeSession: () => runtimeSession,
    services: () => browserServices,
  });
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
    activate: (): (() => void) => {
      const releaseGpt = gptRuntime.activate();
      let releaseNavigation: (() => void) | undefined;
      try {
        releaseNavigation = pageBidsNavigation.activate();
      } catch (error) {
        releaseGpt();
        throw error;
      }
      return (): void => {
        releaseNavigation?.();
        releaseGpt();
      };
    },
    start: (config: unknown): void => {
      gptRuntime.start(config);
      pageBidsNavigation.start();
      const navigation = runtimeSession?.currentNavigation;
      const projection = navigation?.currentAuctionProjection;
      if (navigation && projection) {
        gptProjectionPublisher(
          navigation,
          projection as Readonly<BrowserAuctionProjectionV1>,
          'initial'
        );
      }
    },
  });
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
  const osanoConsentRuntime = createOsanoRuntime();
  const osanoLifecycleRuntime = createOsanoRuntime();
  const permutiveContextRuntime = createPermutiveRuntime({
    registerContext: (contributor) => {
      const registry = auctionContextRegistry;
      const owner = runtimeSession;
      return registry && owner
        ? registerScopedContextContributor(registry, owner, 'permutive_context', contributor)
        : undefined;
    },
  });
  const permutiveLifecycleRuntime = createPermutiveRuntime({
    registerContext: () => undefined,
  });
  const sourcepointConsentRuntime = createSourcepointRuntime();
  const sourcepointLifecycleRuntime = createSourcepointRuntime();
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
  const publishProjectionThroughGpt = async (
    navigation: NonNullable<RuntimeSession['currentNavigation']>,
    projection: Readonly<BrowserAuctionProjectionV1>,
    requestClass: string
  ): Promise<void> => {
    const prepared = preparedBrowserServices;
    const services = browserServices;
    if (!prepared || !services || !navigation.isCurrent() || projection.slots.length === 0) return;
    const physicalBySlot = new Map<
      string,
      Readonly<{ operation: 'display' | 'refresh'; slot: object }>
    >();
    const operation = composition.adapters.googletag.run(
      (gpt) => {
        for (let index = 0; index < projection.slots.length; index += 1) {
          const placement = projection.slots[index];
          if (!placement || !navigation.isCurrent()) break;
          const element = resolveProjectedSlotElement(placement);
          if (!element) continue;
          const definition = Object.freeze({
            adUnitPath: placement.gamUnitPath,
            elementId: element.id,
            sizes: placement.formats,
          });
          const existing = gpt.slots().filter((slot) => gpt.slotElementId?.(slot) === element.id);
          if (existing.length > 1) continue;
          const publisherSlot = existing[0];
          if (publisherSlot) {
            const adopted = services.slots.adoptGptSlot(navigation.generation, placement.slot, {
              definition,
              elementIdPrefix: placement.divId,
              ownership: 'publisher',
              slot: publisherSlot,
            });
            if (adopted.ok) {
              physicalBySlot.set(
                placement.slot,
                Object.freeze({ operation: 'refresh', slot: publisherSlot })
              );
            }
            continue;
          }
          const defined = gpt.transactionalDefine(
            definition,
            () => navigation.isCurrent(),
            (candidate) => {
              let committed = false;
              return Object.freeze({
                commit: (): boolean => {
                  const adopted = services.slots.adoptGptSlot(
                    navigation.generation,
                    placement.slot,
                    {
                      definition,
                      elementIdPrefix: placement.divId,
                      ownership: 'trusted_server',
                      slot: candidate,
                    }
                  );
                  committed = adopted.ok;
                  return committed;
                },
                rollback: (): void => {
                  if (!committed) return;
                  committed = false;
                  services.slots.recordPublisherDestruction(candidate);
                },
              });
            }
          );
          if (defined.status === 'defined') {
            physicalBySlot.set(
              placement.slot,
              Object.freeze({ operation: 'display', slot: defined.slot })
            );
          }
        }
      },
      { signal: navigation.signal }
    );
    try {
      await operation.result;
    } catch (error) {
      if (!navigation.signal.aborted) log.warn('GPT projection: slot binding failed', error);
    }
    if (!navigation.isCurrent()) return;
    const batch = navigation.createAuctionBatch(`gpt:${projection.auction.auctionId}`);
    if (!batch) return;
    let winnerIndex = 0;
    for (let index = 0; index < projection.auction.results.length; index += 1) {
      const decision = projection.auction.results[index];
      const placement = projection.slots[index];
      if (!decision || !placement || decision.outcome !== 'winner') continue;
      const bid = projection.bids[winnerIndex];
      winnerIndex += 1;
      if (!bid || !navigation.isCurrent()) continue;
      if (!('rendererReservationId' in bid)) continue;
      const owner = batch.createRenderAttempt(decision.slot);
      if (!owner.ok) continue;
      const created = prepared.createAttempt(owner.value);
      if (!created.ok) continue;
      const binding = physicalBySlot.get(decision.slot);
      if (!binding) {
        created.value.fail('slot_unresolved');
        continue;
      }
      const artifact = Object.freeze({
        kind: 'puc' as const,
        attemptId: created.value.id,
        slot: created.value.slot,
        navigationGeneration: created.value.navigationGeneration,
        dispose: () => undefined,
      });
      const published = await publishGptWinner({
        artifact,
        attempt: created.value,
        bid,
        createSlotOperation,
        googletag: composition.adapters.googletag,
        navigation,
        operation: binding.operation,
        owner: owner.value,
        placement,
        pucBridge: services.pucBridge,
        requestClass,
        reservations: services.reservations,
        slot: binding.slot,
        slots: services.slots,
        targeting: services.targeting,
        createFallback: (parentAttemptId) => {
          const fallbackOwner = batch.createRenderAttempt(decision.slot);
          if (!fallbackOwner.ok) {
            return Object.freeze({
              ok: false as const,
              reason:
                fallbackOwner.reason === 'identity_generation_failed'
                  ? ('identity_generation_failed' as const)
                  : fallbackOwner.reason === 'stale_owner'
                    ? ('stale_owner' as const)
                    : ('invalid_attempt' as const),
            });
          }
          const fallback = prepared.createAttempt(fallbackOwner.value, parentAttemptId);
          if (!fallback.ok) return fallback;
          if (
            !fallback.value.admitDirectWinner(
              bid.renderSource,
              Object.freeze({ selectedCpm: bid.cpm })
            )
          ) {
            fallback.value.fail('winner_not_renderable');
            return fallback;
          }
          if (!prepared.renderProjectedFallback(fallback.value)) {
            fallback.value.fail('winner_not_renderable');
          }
          return fallback;
        },
      });
      if (!published.ok && navigation.isCurrent()) {
        log.warn('GPT projection: winner publication failed', published.reason);
      }
    }
  };
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
  const runtimeOwner = createRuntime({
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
      creativeBoot = boot.creative;
      diagnosticsBoot = boot.diagnostics;
      const parseProjection = (candidate: unknown): object | undefined =>
        parseBrowserAuctionProjectionV1(candidate);
      const initialProjection = prepareInitialAuctionProjection(
        boot.auctionProjection,
        parseProjection
      );
      if (!initialProjection) throw new Error('Accepted boot projection is unavailable');
      const preparedRenderTrace = createRenderTraceStore({
        onPresentationError: (error) => log.warn('render diagnostics: presentation failed', error),
        onSubscriberError: (error) => log.warn('render diagnostics: subscriber failed', error),
      });
      const preparedDiagnosticsIngress = createDiagnosticsIngress({
        reduce: consumeCoreObservation,
        reportError: (error) => log.warn('diagnostics ingress: reducer failed', error),
      });
      renderTrace = preparedRenderTrace;
      diagnosticsIngress = preparedDiagnosticsIngress;
      const preparedGptDiagnosticsFacts = boot.diagnostics.gpt.active
        ? createGptDiagnosticsFactBuffer({
            onConsumerError: (error) => log.warn('gpt diagnostics: fact consumer failed', error),
          })
        : undefined;
      gptDiagnosticsFacts = preparedGptDiagnosticsFacts;
      context.onDispose(() => {
        preparedGptDiagnosticsFacts?.dispose();
        preparedDiagnosticsIngress.dispose();
        preparedRenderTrace.dispose();
        if (gptDiagnosticsFacts === preparedGptDiagnosticsFacts) {
          gptDiagnosticsFacts = undefined;
        }
        if (diagnosticsIngress === preparedDiagnosticsIngress) diagnosticsIngress = undefined;
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
        prepareRenderSource: (candidate) => {
          const source = parseBidRenderSourceV1(candidate);
          return source?.type === 'pbs_cache' ? undefined : source;
        },
      });
      const bootstrapNonces = createBootstrapNonceRegistry();
      const rendererNonces = createRendererNonceRegistry();
      // A real document origin is authoritative. Only an opaque srcdoc may
      // fall back to the server-stamped base; publisher script must not be able
      // to redirect the APS endpoint by predefining that creative-only stamp.
      const publisherOrigin = trustedDocumentHttpOrigin(window.location.origin);
      if (!publisherOrigin) throw new Error('Trusted publisher origin is unavailable');
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
      const renderDirectAps = Object.freeze(
        (attempt: RenderAttempt, container: HTMLElement): boolean => {
          try {
            return renderDirectApsAttempt({
              attempt,
              bootstrapNonces,
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
      const createOwnedAttempt = (owner: RenderAttemptScope, parentAttemptId?: string) =>
        createRenderAttempt({
          artifacts,
          owner,
          ...(parentAttemptId === undefined ? {} : { parentAttemptId }),
          prepareRenderSource: (candidate) => {
            const source = parseBidRenderSourceV1(candidate);
            return source && source.type !== 'pbs_cache' ? Object.freeze(source) : undefined;
          },
          publishDiagnostics: preparedDiagnosticsIngress.publish,
          reservations: reservationService,
        });
      const renderProjectedFallback = (attempt: RenderAttempt): boolean => {
        const record = slotService.resolveRegisteredSlot(attempt.slot);
        const container = record && resolveDirectContainer(record);
        if (!container) {
          attempt.fail('slot_unresolved');
          return false;
        }
        if (attempt.renderSource?.type === 'aps') return renderDirectAps(attempt, container);
        if (attempt.renderSource?.type === 'adm') return renderDirectAdm(attempt, container);
        attempt.fail('winner_not_renderable');
        return false;
      };
      const batchCoordinator = createAuctionBatchService({
        createAttempt: createOwnedAttempt,
        fetcher: (input, init) => {
          if (typeof fetchAuction !== 'function') return Promise.reject(new Error('unavailable'));
          return fetchAuction(input, init);
        },
        parseResponse: parseTrustedServerAuctionResponseV1,
        renderWinner: renderProjectedFallback,
      });
      const services = Object.freeze({
        artifacts,
        auctionBatches: batchCoordinator,
        bootstrapNonces,
        reservations: reservationService,
        rendererNonces,
        renderDirectAdm,
        renderDirectAps,
        slots: slotService,
        targeting: targetingService,
      });
      preparedBrowserServices = Object.freeze({
        createAttempt: createOwnedAttempt,
        publisherOrigin,
        renderProjectedFallback,
        rendererUrl,
        services,
      });
      const session = createRuntimeSession({
        createIdentityIssuer:
          compositionOptions.createIdentityIssuerForTest ?? createBrowserNavigationIdentityIssuer,
        interfaces: Object.freeze({
          adapters: composition.adapters,
          creative: creativeRuntime,
          datadome: dataDomeRuntime,
          didomi: didomiRuntime,
          google_tag_manager: googleTagManagerRuntime,
          ...(preparedGptDiagnosticsFacts
            ? {
                'gpt.events.v1': Object.freeze({
                  subscribe: preparedGptDiagnosticsFacts.activate,
                }),
              }
            : {}),
          'trace.v1': Object.freeze({
            record: preparedRenderTrace.record,
            enrich: preparedRenderTrace.enrich,
            prune: preparedRenderTrace.prune,
            diagnostics: preparedRenderTrace.diagnostics,
            observations: Object.freeze({
              publish: preparedDiagnosticsIngress.publish,
            }),
          }),
          'trace.presentation.v1': Object.freeze({
            attachPresentation: preparedRenderTrace.attachPresentation,
          }),
          gpt: gptIntegrationRuntime,
          lockr: lockrRuntime,
          osano_consent: osanoConsentRuntime,
          osano_lifecycle: osanoLifecycleRuntime,
          permutive_context: permutiveContextRuntime,
          permutive_lifecycle: permutiveLifecycleRuntime,
          prebid: prebidRuntime,
          sourcepoint_consent: sourcepointConsentRuntime,
          sourcepoint_lifecycle: sourcepointLifecycleRuntime,
          testlight: testlightRuntime,
          ...services,
        }),
        onNavigationDispose: (navigationGeneration) => {
          artifacts.disposeNavigation(navigationGeneration);
          preparedRenderTrace.pruneNavigation(navigationGeneration);
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
        bootstrapNonces.dispose();
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
          creativeBoot = undefined;
          diagnosticsBoot = undefined;
          renderTraceSlotsByNavigation.clear();
        }
      });
      const navigation = session.startInitialNavigation(initialProjection);
      if (!navigation.ok) throw new Error(navigation.reason);

      const acceptedInitialProjection = initialProjection as Readonly<BrowserAuctionProjectionV1>;
      const initialRegistrations = [
        ...acceptedInitialProjection.slots.map((placement) => ({
          domAliases: Object.freeze([placement.divId]),
          registeredSlotId: placement.slot,
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
      const ingress = diagnosticsIngress;
      const pucBridge = createPucBridge({
        messaging: composition.adapters.messaging,
        mintLifecycleTicket: mintBrowserLifecycleTicket,
        mountAps: (input) =>
          renderPucApsAttempt({
            ...input,
            bootstrapNonces: prepared.services.bootstrapNonces,
            messaging: composition.adapters.messaging,
            nonces: prepared.services.rendererNonces,
            publisherOrigin: prepared.publisherOrigin,
          }),
        ...(compositionOptions.pucSchedulerForTest
          ? { scheduler: compositionOptions.pucSchedulerForTest }
          : {}),
        reservations: prepared.services.reservations,
        resizeCollapsedShell: resizeCollapsedPucShell,
        slots: prepared.services.slots,
      });
      context.onDispose(() => pucBridge.dispose());
      browserServices = Object.freeze({ ...prepared.services, pucBridge });
      gptProjectionPublisher = (navigation, projection, requestClass): void => {
        void publishProjectionThroughGpt(navigation, projection, requestClass).catch((error) => {
          if (navigation.isCurrent()) log.warn('GPT projection: coordinator failed', error);
        });
      };
      context.onDispose(() => {
        gptProjectionPublisher = noopGptProjectionPublisher;
      });
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
      if (facts && ingress) {
        const releaseCapture = activateGptDiagnosticsFactCapture(
          composition.adapters.googletag,
          Object.freeze({
            publish: (fact: Readonly<GoogletagDiagnosticsFact>): boolean => {
              const projected = projectGptTraceFact(fact);
              if (projected) ingress.publish(projected);
              return facts.publish(fact);
            },
          })
        );
        if (!releaseCapture) throw new Error('GPT diagnostics capture is unavailable');
        context.onDispose(releaseCapture);
      }
      compositionOptions.coreActivations?.correctnessGptListeners(
        context,
        composition.adapters,
        browserServices
      );
      return runtimeOptions.activateCore?.(context);
    },
  });
  return Object.freeze({
    adapters: composition.adapters,
    runtime: runtimeOwner,
    createDiagnosticsCapabilityProviderRegistrationForTest: () => {
      const prepare = () => {
        const facts = gptDiagnosticsFacts;
        const trace = renderTrace;
        const observations = diagnosticsIngress;
        if (!facts || !trace || !observations) {
          throw new TypeError('Test diagnostics capabilities are unavailable');
        }
        return Object.freeze({
          activate: () => undefined,
          interfaces: Object.freeze({
            'gpt.events.v1': Object.freeze({ subscribe: facts.activate }),
            'trace.v1': Object.freeze({
              record: trace.record,
              enrich: trace.enrich,
              prune: trace.prune,
              diagnostics: trace.diagnostics,
              observations: Object.freeze({
                publish: observations.publish,
              }),
            }),
            'trace.presentation.v1': Object.freeze({
              attachPresentation: trace.attachPresentation,
            }),
          }),
        });
      };
      return Object.freeze({
        abi: 1,
        id: BROWSER_TEST_DIAGNOSTICS_PROVIDER_ID,
        phase: 'takeover',
        releaseId: runtimeOptions.releaseId,
        prepareSync: prepare,
        prepare,
      });
    },
    createTraceCapabilityProviderRegistrationForTest: () => {
      const prepare = () => {
        const trace = renderTrace;
        const observations = diagnosticsIngress;
        if (!trace || !observations) {
          throw new TypeError('Test trace capability is unavailable');
        }
        return Object.freeze({
          activate: () => undefined,
          interfaces: Object.freeze({
            'trace.v1': Object.freeze({
              record: trace.record,
              enrich: trace.enrich,
              prune: trace.prune,
              diagnostics: trace.diagnostics,
              observations: Object.freeze({
                publish: observations.publish,
              }),
            }),
            'trace.presentation.v1': Object.freeze({
              attachPresentation: trace.attachPresentation,
            }),
          }),
        });
      };
      return Object.freeze({
        abi: 1,
        id: BROWSER_TEST_TRACE_PROVIDER_ID,
        phase: 'takeover',
        releaseId: runtimeOptions.releaseId,
        prepareSync: prepare,
        prepare,
      });
    },
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
        | 'createSlotOperation'
        | 'googletag'
        | 'navigation'
        | 'pucBridge'
        | 'reservations'
        | 'slots'
        | 'targeting'
      >
    ): Promise<GptWinnerPublicationResult> => {
      const services = browserServices;
      const navigation = runtimeSession?.currentNavigation;
      if (!services || !navigation) {
        return Promise.resolve(Object.freeze({ ok: false, reason: 'gpt_request_failed' }));
      }
      return publishGptWinner({
        ...input,
        createSlotOperation,
        googletag: composition.adapters.googletag,
        navigation,
        pucBridge: services.pucBridge,
        reservations: services.reservations,
        slots: services.slots,
        targeting: services.targeting,
      });
    },
    startGptSlotOperationForTest: (
      input: Omit<GptSlotOperationInput, 'createSlotOperation' | 'pucBridge' | 'slots'>
    ): SlotOperationCreationResult => {
      const services = browserServices;
      if (!services) return Object.freeze({ ok: false, reason: 'invalid_attempt' });
      return startGptSlotOperation({
        ...input,
        createSlotOperation,
        pucBridge: services.pucBridge,
        slots: services.slots,
      });
    },
  });
}
