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
import {
  parseBidRenderSourceV1,
  parseBrowserAuctionProjectionV1,
} from '../core/contracts/auction_projection';
import { validateApsRenderer } from '../core/contracts/aps_renderer';
import { renderDirectApsAttempt } from '../integrations/aps/render';
import { createBrowserNavigationIdentityIssuer } from '../kernel/identity';
import type { NavigationIdentityIssuerFactory, RuntimeSession } from '../kernel/sessions';
import { createRuntimeSession } from '../kernel/sessions';
import type { CoreActivationContext } from '../kernel/integration_registry';
import { createRuntime, type Runtime, type RuntimeOptions } from '../kernel/runtime';
import { createAuctionContextRegistry, type AuctionContextRegistry } from '../services/context';
import {
  createPageBidsController,
  type PageBidsController,
  prepareInitialAuctionProjection,
} from '../services/projections';
import { createReservationService, type ReservationService } from '../services/reservations';
import {
  createRendererNonceRegistry,
  type RenderAttempt,
  type RendererNonceRegistry,
} from '../services/render';
import { createSlotService, type SlotService } from '../services/slots';
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
  readonly reservations: ReservationService;
  readonly rendererNonces: RendererNonceRegistry;
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
}

export interface BrowserCoreActivations {
  readonly bridgeRecognizer: (
    context: CoreActivationContext,
    adapters: Readonly<BrowserAdapters>
  ) => void;
  readonly correctnessGptListeners: (
    context: CoreActivationContext,
    adapters: Readonly<BrowserAdapters>,
    services: Readonly<BrowserServices>
  ) => void;
}

export interface TestBrowserRuntimeCompositionOptions extends BrowserCompositionOptions {
  readonly coreActivations: BrowserCoreActivations;
  readonly createIdentityIssuerForTest?: NavigationIdentityIssuerFactory;
  readonly admittedProgrammaticSlotsForTest?: readonly string[];
}

interface AcceptedBrowserBoot {
  readonly auctionProjection: object;
  readonly cachePolicy?: unknown;
  readonly manifest: {
    readonly integrations: readonly { readonly id: string }[];
  };
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
  let runtimeSession: RuntimeSession | undefined;
  let browserServices: Readonly<BrowserServices> | undefined;
  let auctionContextRegistry: AuctionContextRegistry | undefined;
  let projectionParser: ((candidate: unknown) => object | undefined) | undefined;
  const runtime = createRuntime({
    ...runtimeOptions,
    activateOwner: (context) => {
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
      const slotService = createSlotService({ googletag: composition.adapters.googletag });
      const targetingService = createTargetingService();
      const reservationService = createReservationService({
        prepareRenderSource: (candidate) => parseBidRenderSourceV1(candidate, cachePolicy),
      });
      const rendererNonces = createRendererNonceRegistry();
      const publisherOrigin = window.location.origin;
      const renderDirectAps = (attempt: RenderAttempt, container: HTMLElement): boolean => {
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
      };
      const services = Object.freeze({
        reservations: reservationService,
        rendererNonces,
        renderDirectAps,
        slots: slotService,
        targeting: targetingService,
      });
      const session = createRuntimeSession({
        createIdentityIssuer:
          compositionOptions.createIdentityIssuerForTest ?? createBrowserNavigationIdentityIssuer,
        interfaces: Object.freeze({ adapters: composition.adapters, ...services }),
      });
      context.onDispose(() => {
        session.dispose();
        reservationService.dispose();
        rendererNonces.dispose();
        slotService.dispose();
        targetingService.dispose();
        composition.adapters.googletag.dispose();
        composition.adapters.prebid.dispose();
        if (runtimeSession === session) {
          runtimeSession = undefined;
          browserServices = undefined;
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
        runtimeOwner: session,
      });
      runtimeSession = session;
      browserServices = services;
      auctionContextRegistry = contextRegistry;
      projectionParser = parseProjection;
      return runtimeOptions.activateOwner?.(context);
    },
    activateCore: (context) => {
      if (!browserServices) throw new Error('Browser services are unavailable');
      compositionOptions.coreActivations.bridgeRecognizer(context, composition.adapters);
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
  });
}
