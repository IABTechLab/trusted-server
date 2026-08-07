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
} from '../adapters/prebid';
import { parseCacheFetchPolicyV1 } from '../core/config';
import { parseTrustedServerAuctionResponseV1 } from '../core/auction';
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
import { startGptSlotOperation, type GptSlotOperationInput } from '../integrations/gpt/module';
import { createBrowserNavigationIdentityIssuer } from '../kernel/identity';
import type { NavigationIdentityIssuerFactory, RuntimeSession } from '../kernel/sessions';
import { createRuntimeSession } from '../kernel/sessions';
import type { CoreActivationContext } from '../kernel/integration_registry';
import { createRuntime, type Runtime, type RuntimeOptions } from '../kernel/runtime';
import { createAuctionContextRegistry, type AuctionContextRegistry } from '../services/context';
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
  readonly createIdentityIssuerForTest?: NavigationIdentityIssuerFactory;
  readonly admittedProgrammaticSlotsForTest?: readonly string[];
  readonly gptStartupForTest?: (config: unknown) => void;
}

interface AcceptedBrowserBoot {
  readonly auctionProjection: object;
  readonly cachePolicy?: unknown;
  readonly manifest: {
    readonly integrations: readonly { readonly id: string }[];
  };
}

interface PreparedBrowserServices {
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
  const startGpt = compositionOptions.gptStartupForTest ?? (() => undefined);
  const gptRuntime = Object.freeze({ start: startGpt });
  let runtimeSession: RuntimeSession | undefined;
  const getBindings: NonNullable<RuntimeOptions['getBindings']> = (id) => {
    const provided = providedBindings?.(id);
    let config: unknown;
    if (provided !== undefined) {
      const descriptor = Object.getOwnPropertyDescriptor(provided, 'config');
      if (!descriptor || !('value' in descriptor)) return provided;
      config = descriptor.value;
    }
    const interfaces = runtimeSession?.interfaces;
    if (!interfaces) throw new Error(`Integration interfaces are unavailable for ${id}`);
    return Object.freeze({
      config,
      interfaces,
    });
  };
  let preparedBrowserServices: PreparedBrowserServices | undefined;
  let browserServices: Readonly<BrowserServices> | undefined;
  let auctionContextRegistry: AuctionContextRegistry | undefined;
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
    kernel: {
      addAdUnits: addProgrammaticAdUnits,
      diagnostics: runtimeOptions.kernel.diagnostics,
      requestAds: requestDirectAds,
    },
    prepareOwner: (context) => {
      const boot = context.boot as unknown as AcceptedBrowserBoot;
      const cachePolicy =
        boot.cachePolicy === undefined ? undefined : parseCacheFetchPolicyV1(boot.cachePolicy);
      const parseProjection = (candidate: unknown): object | undefined =>
        parseBrowserAuctionProjectionV1(candidate, boot.cachePolicy);
      const initialProjection = prepareInitialAuctionProjection(
        boot.auctionProjection,
        parseProjection
      );
      if (!initialProjection) throw new Error('Accepted boot projection is unavailable');
      const reconciliation =
        typeof document === 'undefined' || typeof MutationObserver === 'undefined'
          ? undefined
          : createBrowserSlotReconciliationBoundary(document, MutationObserver);
      const slotService = createSlotService({
        googletag: composition.adapters.googletag,
        ...(reconciliation ? { reconciliation } : {}),
      });
      const targetingService = createTargetingService();
      const reservationService = createReservationService({
        prepareRenderSource: (candidate) => parseBidRenderSourceV1(candidate, cachePolicy),
      });
      const artifacts = createCommittedArtifactStore();
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
            publisherOrigin,
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
      const batchCoordinator = createAuctionBatchService({
        ...(cachePolicy ? { cachePolicy } : {}),
        createAttempt: (owner) =>
          createRenderAttempt({
            artifacts,
            owner,
            prepareRenderSource: (candidate) => {
              const source = parseBidRenderSourceV1(candidate, cachePolicy);
              return source ? Object.freeze(source) : undefined;
            },
            reservations: reservationService,
          }),
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
        publisherOrigin,
        rendererUrl,
        resolveCacheAdm,
        services,
      });
      const session = createRuntimeSession({
        createIdentityIssuer:
          compositionOptions.createIdentityIssuerForTest ?? createBrowserNavigationIdentityIssuer,
        interfaces: Object.freeze({ adapters: composition.adapters, gpt: gptRuntime, ...services }),
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
      const pucBridge = createPucBridge({
        messaging: composition.adapters.messaging,
        publisherOrigin: prepared.publisherOrigin,
        rendererNonces: prepared.services.rendererNonces,
        rendererUrl: prepared.rendererUrl,
        reservations: prepared.services.reservations,
        resizeCollapsedShell: resizeCollapsedPucShell,
        resolveCacheAdm: prepared.resolveCacheAdm,
      });
      context.onDispose(() => pucBridge.dispose());
      browserServices = Object.freeze({ ...prepared.services, pucBridge });
      browserServices.slots.activate();
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
