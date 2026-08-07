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
} from '../adapters/messaging';
import {
  createBrowserPrebidAdapter,
  createNoopPrebidAdapter,
  type PrebidAdapter,
  type PrebidGlobalTarget,
} from '../adapters/prebid';
import { parseBrowserAuctionProjectionV1 } from '../core/contracts/auction_projection';
import { createBrowserNavigationIdentityIssuer } from '../kernel/identity';
import type { NavigationIdentityIssuerFactory, RuntimeSession } from '../kernel/sessions';
import { createRuntimeSession } from '../kernel/sessions';
import type { CoreActivationContext } from '../kernel/integration_registry';
import { createRuntime, type Runtime, type RuntimeOptions } from '../kernel/runtime';
import { createAuctionContextRegistry, type AuctionContextRegistry } from '../services/context';
import {
  createPageBidsController,
  type PageBidsController,
  type PreparedProjectionSlots,
  type ProjectionSlotRegistry,
  prepareInitialAuctionProjection,
} from '../services/projections';

export interface BrowserAdapters {
  readonly googletag: GoogletagAdapter;
  readonly messaging: MessagingAdapter;
  readonly prebid: PrebidAdapter;
}

export interface BrowserComposition {
  readonly adapters: Readonly<BrowserAdapters>;
}

export type BrowserAdapterTarget = GoogletagGlobalTarget & PrebidGlobalTarget & MessageEventTarget;

export interface BrowserCompositionOptions {
  readonly adapters?: Partial<BrowserAdapters>;
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
}

export interface BrowserCoreActivations {
  readonly bridgeRecognizer: (
    context: CoreActivationContext,
    adapters: Readonly<BrowserAdapters>
  ) => void;
  readonly correctnessGptListeners: (
    context: CoreActivationContext,
    adapters: Readonly<BrowserAdapters>
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

class BrowserProjectionSlotLedger {
  private readonly slots = new Map<string, object>();

  public bind(
    navigation: NonNullable<RuntimeSession['currentNavigation']>
  ): ProjectionSlotRegistry {
    return Object.freeze({
      prepareProjectionSlots: (
        ownerGeneration: object,
        slots: readonly string[],
        maximumActiveSlots: number
      ): PreparedProjectionSlots | undefined => {
        const ownedSlots = Object.freeze([...slots]);
        if (
          ownerGeneration !== navigation.generation ||
          !navigation.isCurrent() ||
          ownedSlots.some((slot) => typeof slot !== 'string') ||
          new Set(ownedSlots).size !== ownedSlots.length ||
          this.slots.size + ownedSlots.length > maximumActiveSlots ||
          ownedSlots.some((slot) => this.slots.has(slot))
        ) {
          return undefined;
        }
        let active = false;
        let ownerDisposed = false;
        const rollback = (): void => {
          if (!active) return;
          active = false;
          for (const slot of ownedSlots) {
            if (this.slots.get(slot) === ownerGeneration) this.slots.delete(slot);
          }
        };
        return Object.freeze({
          ownerGeneration,
          commit: (): boolean => {
            if (
              active ||
              ownerDisposed ||
              !navigation.isCurrent() ||
              this.slots.size + ownedSlots.length > maximumActiveSlots ||
              ownedSlots.some((slot) => this.slots.has(slot))
            ) {
              return false;
            }
            navigation.onDispose('projection-slots', () => {
              ownerDisposed = true;
              rollback();
            });
            if (ownerDisposed || !navigation.isCurrent()) return false;
            for (const slot of ownedSlots) this.slots.set(slot, ownerGeneration);
            active = true;
            return true;
          },
          rollback,
        });
      },
    });
  }

  public seed(
    navigation: NonNullable<RuntimeSession['currentNavigation']>,
    slots: readonly string[]
  ): boolean {
    const reservation = this.bind(navigation).prepareProjectionSlots(
      navigation.generation,
      slots,
      256
    );
    return reservation?.commit() ?? false;
  }

  public admitProgrammatic(
    navigation: NonNullable<RuntimeSession['currentNavigation']>,
    slots: readonly string[]
  ): boolean {
    const reservation = this.bind(navigation).prepareProjectionSlots(
      navigation.generation,
      slots,
      256
    );
    return reservation?.commit() ?? false;
  }

  public snapshotForTest(): readonly string[] {
    return Object.freeze([...this.slots.keys()]);
  }
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
  const googletag =
    options.adapters?.googletag ??
    (options.target
      ? createBrowserGoogletagAdapter(options.target)
      : createBrowserGoogletagAdapter());
  const messaging =
    options.adapters?.messaging ??
    (options.target
      ? createBrowserMessagingAdapter(options.target)
      : createBrowserMessagingAdapter());
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
  let projectionSlotLedger: BrowserProjectionSlotLedger | undefined;
  let auctionContextRegistry: AuctionContextRegistry | undefined;
  let projectionParser: ((candidate: unknown) => object | undefined) | undefined;
  const runtime = createRuntime({
    ...runtimeOptions,
    activateOwner: (context) => {
      const boot = context.boot as unknown as AcceptedBrowserBoot;
      const parseProjection = (candidate: unknown): object | undefined =>
        parseBrowserAuctionProjectionV1(candidate, boot.cachePolicy);
      const initialProjection = prepareInitialAuctionProjection(
        boot.auctionProjection,
        parseProjection
      );
      if (!initialProjection) throw new Error('Accepted boot projection is unavailable');
      const session = createRuntimeSession({
        createIdentityIssuer:
          compositionOptions.createIdentityIssuerForTest ?? createBrowserNavigationIdentityIssuer,
        interfaces: Object.freeze({ adapters: composition.adapters }),
      });
      context.onDispose(() => {
        session.dispose();
        if (runtimeSession === session) {
          runtimeSession = undefined;
          projectionSlotLedger = undefined;
          auctionContextRegistry = undefined;
          projectionParser = undefined;
        }
      });
      const navigation = session.startInitialNavigation(initialProjection);
      if (!navigation.ok) throw new Error(navigation.reason);

      const ledger = new BrowserProjectionSlotLedger();
      if (
        !ledger.admitProgrammatic(
          navigation.value,
          compositionOptions.admittedProgrammaticSlotsForTest ?? []
        )
      ) {
        throw new Error('Initial programmatic slots exceed the shared registry');
      }
      if (!ledger.seed(navigation.value, projectionSlots(initialProjection))) {
        throw new Error('Initial projection slots exceed the shared registry');
      }
      const contextRegistry = createAuctionContextRegistry({
        manifestIntegrationIds: Object.freeze(boot.manifest.integrations.map(({ id }) => id)),
        runtimeOwner: session,
      });
      runtimeSession = session;
      projectionSlotLedger = ledger;
      auctionContextRegistry = contextRegistry;
      projectionParser = parseProjection;
      return runtimeOptions.activateOwner?.(context);
    },
    activateCore: (context) => {
      compositionOptions.coreActivations.bridgeRecognizer(context, composition.adapters);
      compositionOptions.coreActivations.correctnessGptListeners(context, composition.adapters);
      return runtimeOptions.activateCore?.(context);
    },
  });
  return Object.freeze({
    adapters: composition.adapters,
    runtime,
    runtimeSessionForTest: () => runtimeSession,
    pageBidsControllerForTest: (): PageBidsController | undefined => {
      const navigation = runtimeSession?.currentNavigation;
      if (!navigation || !projectionSlotLedger || !projectionParser) return undefined;
      return createPageBidsController({
        navigation,
        parseProjection: projectionParser,
        slotRegistry: projectionSlotLedger.bind(navigation),
      });
    },
    projectionSlotsForTest: () => projectionSlotLedger?.snapshotForTest(),
    auctionContextRegistryForTest: () => auctionContextRegistry,
  });
}
