import {
  isRendererReservationIdV1,
  ownDataArray,
  ownDataObject,
  validBoundedString,
  validDimension,
} from '../../core/contracts/auction_projection';
import type { BrowserAuctionBidV1, BrowserAuctionProjectionV1 } from '../../core/types';
import {
  PrebidAdmissionContractError,
  type PrebidAdapter,
  type PrebidEventFacade,
  type PrebidOperation,
  type PreparedTrustedBidV1,
} from '../../adapters/prebid';
import type {
  GoogletagAdapter,
  GoogletagOperation,
  GoogletagPublisherRefreshCall,
} from '../../adapters/googletag';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import type {
  AuctionBatchScope,
  NavigationSession,
  RenderAttemptScope,
} from '../../kernel/sessions';
import type {
  RenderAttempt,
  RenderAttemptCreationResult,
  RenderScheduler,
} from '../../services/render';
import type { ReservationService } from '../../services/reservations';

export const PREBID_INTEGRATION_ID = 'prebid' as const;
const TRUSTED_SERVER_PREBID_BIDDER = 'trustedServer';
export type { PreparedTrustedBidV1 } from '../../adapters/prebid';

const MAX_CONFIG_DEPTH = 16;
const MAX_CONFIG_NODES = 512;
const MAX_CONFIG_MEMBERS = 256;
const arrayIsArrayIntrinsic = Array.isArray;
const numberIsFiniteIntrinsic = Number.isFinite;
const objectGetOwnPropertyDescriptorIntrinsic = Object.getOwnPropertyDescriptor;
const objectGetOwnPropertyNamesIntrinsic = Object.getOwnPropertyNames;
const objectGetOwnPropertySymbolsIntrinsic = Object.getOwnPropertySymbols;
const objectGetPrototypeOfIntrinsic = Object.getPrototypeOf;
const objectIsFrozenIntrinsic = Object.isFrozen;

interface PrebidIntegrationRuntime {
  readonly activate: () => () => void;
  readonly start: (config: unknown) => void;
}

function validFrozenConfig(candidate: unknown): boolean {
  const seen = new Set<object>();
  let nodes = 0;
  const visit = (value: unknown, depth: number, topLevel: boolean): boolean => {
    if (value === undefined) return topLevel;
    if (value === null || typeof value === 'string' || typeof value === 'boolean') return true;
    if (typeof value === 'number') return numberIsFiniteIntrinsic(value);
    if (typeof value !== 'object' || depth > MAX_CONFIG_DEPTH || nodes >= MAX_CONFIG_NODES) {
      return false;
    }
    if (seen.has(value) || !objectIsFrozenIntrinsic(value)) return false;
    seen.add(value);
    nodes += 1;

    const array = arrayIsArrayIntrinsic(value);
    const prototype = objectGetPrototypeOfIntrinsic(value);
    if (
      (!array && prototype !== Object.prototype && prototype !== null) ||
      (array && prototype !== Array.prototype)
    ) {
      return false;
    }
    if (objectGetOwnPropertySymbolsIntrinsic(value).length !== 0) return false;
    const names = objectGetOwnPropertyNamesIntrinsic(value);
    if (names.length > MAX_CONFIG_MEMBERS + (array ? 1 : 0)) return false;
    if (array) {
      const length = objectGetOwnPropertyDescriptorIntrinsic(value, 'length');
      if (!length || !('value' in length) || names.length !== length.value + 1) return false;
      for (let index = 0; index < length.value; index += 1) {
        const descriptor = objectGetOwnPropertyDescriptorIntrinsic(value, String(index));
        if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return false;
        if (!visit(descriptor.value, depth + 1, false)) return false;
      }
      return true;
    }

    for (let index = 0; index < names.length; index += 1) {
      const name = names[index];
      if (name === undefined) return false;
      const descriptor = objectGetOwnPropertyDescriptorIntrinsic(value, name);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return false;
      if (!visit(descriptor.value, depth + 1, false)) return false;
    }
    return true;
  };

  try {
    return visit(candidate, 0, true);
  } catch {
    return false;
  }
}

function readPrebidRuntime(
  interfaces: Readonly<Record<string, unknown>>
): PrebidIntegrationRuntime | undefined {
  try {
    const descriptor = objectGetOwnPropertyDescriptorIntrinsic(interfaces, PREBID_INTEGRATION_ID);
    if (!descriptor || !('value' in descriptor)) return undefined;
    const candidate = descriptor.value;
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      arrayIsArrayIntrinsic(candidate) ||
      !objectIsFrozenIntrinsic(candidate) ||
      Reflect.ownKeys(candidate).length !== 2
    ) {
      return undefined;
    }
    const activate = objectGetOwnPropertyDescriptorIntrinsic(candidate, 'activate');
    const start = objectGetOwnPropertyDescriptorIntrinsic(candidate, 'start');
    if (
      !activate ||
      !('value' in activate) ||
      typeof activate.value !== 'function' ||
      !start ||
      !('value' in start) ||
      typeof start.value !== 'function'
    ) {
      return undefined;
    }
    return candidate as PrebidIntegrationRuntime;
  } catch {
    return undefined;
  }
}

/** Build the release-bound Prebid module registered by the coordinated runtime. */
export function createPrebidIntegrationRegistration(release: string): IntegrationRegistration {
  return Object.freeze({
    id: PREBID_INTEGRATION_ID,
    release,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      if (!validFrozenConfig(config)) throw new TypeError('Prebid integration config is invalid');
      const runtime = readPrebidRuntime(interfaces);
      if (!runtime) throw new TypeError('Prebid integration runtime is unavailable');

      return Object.freeze({
        activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
          const runtimeRelease: { value?: () => void } = {};
          onDispose(() => runtimeRelease.value?.());
          const release = runtime.activate();
          if (typeof release !== 'function') {
            throw new TypeError('Prebid integration activation disposer is unavailable');
          }
          runtimeRelease.value = release;
          afterCommit(() => runtime.start(config));
        },
      });
    },
  });
}

const PREBID_REFRESH_TIMEOUT_MS = 1_500;
const MAX_PREBID_REFRESH_AD_UNITS = 64;
const PREBID_REFRESH_TARGETING_KEYS = Object.freeze([
  'ts_initial',
  'hb_pb',
  'hb_bidder',
  'hb_adid',
  'hb_cache_host',
  'hb_cache_path',
]);

export interface PrebidRefreshPolicyOptions {
  readonly currentNavigation: () => NavigationSession | undefined;
  readonly excludedGamAdUnitPathSuffixes: readonly string[] | (() => readonly string[]);
  readonly googletag: Pick<GoogletagAdapter, 'run'>;
  readonly runSyntheticAuction: (
    slots: readonly object[],
    navigation: NavigationSession
  ) => PrebidRefreshAuctionOperation;
}

export interface PrebidRefreshAuctionPreparation {
  readonly adUnitCodes: readonly string[];
  readonly adUnits: readonly object[];
}

export interface PrebidRegisteredRefreshAuctionOptions {
  readonly clientSideBidders: readonly string[];
  readonly resolveAdUnit: (slot: object) => unknown;
  readonly slots: readonly object[];
}

export interface PrebidRefreshAuctionOperation {
  readonly completion: Promise<void>;
  readonly dispose: () => void;
}

export interface PrebidSyntheticRefreshRunnerOptions {
  readonly prebid: Pick<PrebidAdapter, 'run'>;
  readonly prepareAuction: (slots: readonly object[], navigation: NavigationSession) => unknown;
  readonly scheduler?: RenderScheduler;
}

export type PrebidSyntheticRefreshRunner = (
  slots: readonly object[],
  navigation: NavigationSession
) => PrebidRefreshAuctionOperation;

export interface PrebidRefreshPolicy {
  readonly dispose: () => void;
  readonly prepare: (
    call: Readonly<GoogletagPublisherRefreshCall>
  ) => PromiseLike<unknown> | undefined;
}

interface PrebidRefreshNavigationOwner {
  active: boolean;
  readonly navigation: NavigationSession;
  readonly pending: Set<PrebidPendingRefresh>;
}

interface PrebidPendingRefresh {
  active: boolean;
  auctionOperation: PrebidRefreshAuctionOperation | undefined;
  readonly owner: PrebidRefreshNavigationOwner;
  operation: GoogletagOperation<readonly object[]> | undefined;
  readonly resolve: () => void;
  readonly settle: () => void;
}

function defaultRefreshScheduler(): RenderScheduler {
  return Object.freeze({
    clear: (handle: unknown): void => {
      globalThis.clearTimeout(handle as ReturnType<typeof globalThis.setTimeout>);
    },
    set: (callback: () => void, milliseconds: number): unknown =>
      globalThis.setTimeout(callback, milliseconds),
  });
}

function validRefreshAuctionPreparation(
  candidate: unknown
): PrebidRefreshAuctionPreparation | undefined {
  try {
    const record = ownDataObject(candidate);
    if (!record || !Array.isArray(record.adUnits) || !Array.isArray(record.adUnitCodes)) {
      return undefined;
    }
    if (
      !Object.isFrozen(record.adUnits) ||
      !Object.isFrozen(record.adUnitCodes) ||
      record.adUnits.length === 0 ||
      record.adUnits.length > MAX_PREBID_REFRESH_AD_UNITS ||
      record.adUnits.length !== record.adUnitCodes.length
    ) {
      return undefined;
    }
    const codes = new Set<string>();
    for (let index = 0; index < record.adUnits.length; index += 1) {
      const code = record.adUnitCodes[index];
      const adUnit = ownDataObject(record.adUnits[index]);
      if (!validBoundedString(code, 128) || codes.has(code) || !adUnit || adUnit.code !== code) {
        return undefined;
      }
      codes.add(code);
    }
    return record as unknown as PrebidRefreshAuctionPreparation;
  } catch {
    return undefined;
  }
}

function defineDataProperty(target: Record<string, unknown>, key: string, value: unknown): void {
  Object.defineProperty(target, key, {
    configurable: false,
    enumerable: true,
    value,
    writable: false,
  });
}

/**
 * Rebuild synthetic Prebid units from detached runtime-owned registrations.
 *
 * The composition root resolves physical GPT identities to registered units;
 * this integration-owned boundary performs all bidder routing without reading
 * mutable `pbjs.adUnits` publisher state.
 */
export function preparePrebidRegisteredRefreshAuction(
  options: PrebidRegisteredRefreshAuctionOptions
): PrebidRefreshAuctionPreparation | undefined {
  try {
    if (
      options.slots.length === 0 ||
      options.slots.length > MAX_PREBID_REFRESH_AD_UNITS ||
      !Object.isFrozen(options.slots)
    ) {
      return undefined;
    }
    const clientSideBidders = new Set<string>();
    for (let index = 0; index < options.clientSideBidders.length; index += 1) {
      const bidder = options.clientSideBidders[index];
      if (!validBoundedString(bidder, 64)) return undefined;
      clientSideBidders.add(bidder);
    }

    const adUnitCodes: string[] = [];
    const adUnits: object[] = [];
    const seenCodes = new Set<string>();
    for (let slotIndex = 0; slotIndex < options.slots.length; slotIndex += 1) {
      const slot = options.slots[slotIndex];
      if (!slot) return undefined;
      const source = ownDataObject(options.resolveAdUnit(slot));
      if (!source || !validBoundedString(source.code, 128) || seenCodes.has(source.code)) {
        return undefined;
      }
      const mediaTypes = ownDataObject(source.mediaTypes);
      if (!mediaTypes || !Object.isFrozen(source.mediaTypes)) return undefined;
      const rawBids =
        source.bids === undefined ? [] : ownDataArray(source.bids, MAX_CONFIG_MEMBERS);
      if (!rawBids || (source.bids !== undefined && !Object.isFrozen(source.bids))) {
        return undefined;
      }

      const bidderParamEntries = new Map<string, unknown>();
      const trustedParams: Record<string, unknown> = {};
      const clientBids: object[] = [];
      let foundTrustedBid = false;
      for (let bidIndex = 0; bidIndex < rawBids.length; bidIndex += 1) {
        const bid = ownDataObject(rawBids[bidIndex]);
        if (!bid || !validBoundedString(bid.bidder, 64)) return undefined;
        const params = bid.params === undefined ? Object.freeze({}) : bid.params;
        if (!ownDataObject(params) || !Object.isFrozen(params)) return undefined;
        if (bid.bidder === TRUSTED_SERVER_PREBID_BIDDER) {
          if (foundTrustedBid) return undefined;
          foundTrustedBid = true;
          const existingParams = ownDataObject(params);
          if (!existingParams) return undefined;
          for (const [key, value] of Object.entries(existingParams)) {
            if (key !== 'bidderParams') defineDataProperty(trustedParams, key, value);
          }
          const folded = existingParams['bidderParams'];
          if (folded !== undefined) {
            const foldedRecord = ownDataObject(folded);
            if (!foldedRecord || !Object.isFrozen(folded)) return undefined;
            for (const [bidder, bidderValue] of Object.entries(foldedRecord)) {
              if (!validBoundedString(bidder, 64) || !ownDataObject(bidderValue)) return undefined;
              if (!clientSideBidders.has(bidder)) bidderParamEntries.set(bidder, bidderValue);
            }
          }
          continue;
        }
        if (clientSideBidders.has(bid.bidder)) {
          clientBids.push(Object.freeze({ bidder: bid.bidder, params }));
          continue;
        }
        bidderParamEntries.set(bid.bidder, params);
      }

      const bidderParams: Record<string, unknown> = {};
      for (const [bidder, params] of bidderParamEntries) {
        defineDataProperty(bidderParams, bidder, params);
      }
      defineDataProperty(trustedParams, 'bidderParams', Object.freeze(bidderParams));
      const synthetic = Object.freeze({
        code: source.code,
        mediaTypes: source.mediaTypes,
        bids: Object.freeze([
          Object.freeze({
            bidder: TRUSTED_SERVER_PREBID_BIDDER,
            params: Object.freeze(trustedParams),
          }),
          ...clientBids,
        ]),
      });
      seenCodes.add(source.code);
      adUnitCodes.push(source.code);
      adUnits.push(synthetic);
    }
    return Object.freeze({
      adUnitCodes: Object.freeze(adUnitCodes),
      adUnits: Object.freeze(adUnits),
    });
  } catch {
    return undefined;
  }
}

/** Run one synthetic refresh auction through the exact current Prebid adapter binding. */
export function createPrebidSyntheticRefreshRunner(
  options: PrebidSyntheticRefreshRunnerOptions
): PrebidSyntheticRefreshRunner {
  const scheduler = options.scheduler ?? defaultRefreshScheduler();
  return (slots, navigation): PrebidRefreshAuctionOperation => {
    let active = true;
    let adapterOperation: PrebidOperation<unknown> | undefined;
    let timer: unknown;
    let timerArmed = false;
    let resolveCompletion!: () => void;
    const completion = new Promise<void>((resolve) => {
      resolveCompletion = resolve;
    });
    const settle = (): void => {
      if (!active) return;
      active = false;
      if (timerArmed) {
        timerArmed = false;
        try {
          scheduler.clear(timer);
        } catch {
          // The runner's logical completion remains terminal.
        }
      }
      const operation = adapterOperation;
      adapterOperation = undefined;
      try {
        operation?.dispose();
      } catch {
        // Adapter cleanup cannot prevent the deferred GPT call from resuming.
      }
      resolveCompletion();
    };
    const handle = Object.freeze({ completion, dispose: settle });

    let prepared: PrebidRefreshAuctionPreparation | undefined;
    try {
      if (!navigation.isCurrent()) {
        settle();
        return handle;
      }
      prepared = validRefreshAuctionPreparation(
        options.prepareAuction(Object.freeze([...slots]), navigation)
      );
    } catch {
      prepared = undefined;
    }
    if (!prepared) {
      settle();
      return handle;
    }

    try {
      const codes = Object.freeze([...prepared.adUnitCodes]);
      const adUnits = Object.freeze([...prepared.adUnits]);
      const operation = options.prebid.run(
        (prebid) =>
          new Promise<void>((resolveRequest) => {
            let requestActive = true;
            const finishRequest = (applyTargeting: boolean): void => {
              if (!requestActive) return;
              requestActive = false;
              if (timerArmed) {
                timerArmed = false;
                try {
                  scheduler.clear(timer);
                } catch {
                  // The request completion latch remains terminal.
                }
              }
              if (active && applyTargeting) {
                try {
                  prebid.setTargetingForGpt(codes);
                } catch {
                  // Targeting failure still resumes the exact deferred GPT request.
                }
              }
              resolveRequest();
            };
            try {
              prebid.requestBids(
                Object.freeze({
                  adUnits,
                  bidsBackHandler: () => finishRequest(true),
                  timeout: PREBID_REFRESH_TIMEOUT_MS,
                })
              );
            } catch {
              finishRequest(false);
              return;
            }
            if (!requestActive || !active) return;
            let installedTimer: unknown;
            try {
              installedTimer = scheduler.set(() => finishRequest(true), PREBID_REFRESH_TIMEOUT_MS);
              if (requestActive && active) {
                timer = installedTimer;
                timerArmed = true;
              } else {
                try {
                  scheduler.clear(installedTimer);
                } catch {
                  // A synchronously-fired timeout is already terminal.
                }
              }
            } catch {
              finishRequest(true);
            }
          }),
        Object.freeze({ signal: navigation.signal })
      );
      adapterOperation = operation;
      if (!active) {
        try {
          operation.dispose();
        } catch {
          // The runner's exact completion latch has already settled.
        }
      } else {
        void operation.result.then(settle, settle);
      }
    } catch {
      settle();
    }
    return handle;
  };
}

/** Defer one publisher refresh through navigation-owned targeting cleanup and Prebid work. */
export function createPrebidRefreshPolicy(
  options: PrebidRefreshPolicyOptions
): PrebidRefreshPolicy {
  const owners = new WeakMap<NavigationSession, PrebidRefreshNavigationOwner>();
  const pending = new Set<PrebidPendingRefresh>();
  let disposed = false;

  const currentNavigation = (): NavigationSession | undefined => {
    try {
      const navigation = options.currentNavigation();
      return navigation?.isCurrent() ? navigation : undefined;
    } catch {
      return undefined;
    }
  };

  const ownerFor = (navigation: NavigationSession): PrebidRefreshNavigationOwner | undefined => {
    const current = owners.get(navigation);
    if (current?.active) return current;
    const owner: PrebidRefreshNavigationOwner = {
      active: true,
      navigation,
      pending: new Set(),
    };
    try {
      navigation.onDispose('prebid-refresh-policy', () => {
        owner.active = false;
        owners.delete(navigation);
        const snapshot = [...owner.pending];
        for (let index = 0; index < snapshot.length; index += 1) snapshot[index]?.settle();
      });
    } catch {
      return undefined;
    }
    if (!navigation.isCurrent()) return undefined;
    owners.set(navigation, owner);
    return owner;
  };

  const prepare = (
    call: Readonly<GoogletagPublisherRefreshCall>
  ): PromiseLike<unknown> | undefined => {
    if (disposed) return undefined;
    const navigation = currentNavigation();
    if (!navigation) return undefined;
    let slots: readonly object[];
    let suffixes: readonly string[];
    try {
      if (!Array.isArray(call.slots)) return undefined;
      const snapshot: object[] = [];
      for (let index = 0; index < call.slots.length; index += 1) {
        const slot = call.slots[index];
        if ((typeof slot !== 'object' && typeof slot !== 'function') || slot === null) {
          return undefined;
        }
        snapshot.push(slot);
      }
      slots = Object.freeze(snapshot);
    } catch {
      return undefined;
    }
    try {
      const configuredSuffixes =
        typeof options.excludedGamAdUnitPathSuffixes === 'function'
          ? options.excludedGamAdUnitPathSuffixes()
          : options.excludedGamAdUnitPathSuffixes;
      suffixes = Object.freeze([...configuredSuffixes]);
    } catch {
      suffixes = Object.freeze([]);
    }
    const owner = ownerFor(navigation);
    if (!owner) return undefined;

    let resolveCompletion!: () => void;
    const completion = new Promise<void>((resolve) => {
      resolveCompletion = resolve;
    });
    const requestReference: { value?: PrebidPendingRefresh } = {};
    const settle = (): void => {
      const request = requestReference.value;
      if (!request?.active) return;
      request.active = false;
      pending.delete(request);
      request.owner.pending.delete(request);
      const operation = request.operation;
      request.operation = undefined;
      try {
        operation?.dispose();
      } catch {
        // GPT cleanup failure cannot prevent the publisher refresh from resuming.
      }
      const auctionOperation = request.auctionOperation;
      request.auctionOperation = undefined;
      try {
        auctionOperation?.dispose();
      } catch {
        // Prebid cleanup failure cannot prevent the publisher refresh from resuming.
      }
      request.resolve();
    };
    const request: PrebidPendingRefresh = {
      active: true,
      auctionOperation: undefined,
      operation: undefined,
      owner,
      resolve: resolveCompletion,
      settle,
    };
    requestReference.value = request;
    pending.add(request);
    owner.pending.add(request);

    try {
      const operation = options.googletag.run((gpt) => {
        const eligible: object[] = [];
        for (let slotIndex = 0; slotIndex < slots.length; slotIndex += 1) {
          const slot = slots[slotIndex];
          if (!slot) continue;
          let clearFailed = false;
          for (let keyIndex = 0; keyIndex < PREBID_REFRESH_TARGETING_KEYS.length; keyIndex += 1) {
            try {
              gpt.clearTargeting(slot, PREBID_REFRESH_TARGETING_KEYS[keyIndex]);
            } catch {
              clearFailed = true;
            }
          }
          if (clearFailed) {
            eligible.push(slot);
            continue;
          }
          let adUnitPath: unknown;
          try {
            adUnitPath = gpt.adUnitPath?.(slot);
          } catch {
            eligible.push(slot);
            continue;
          }
          if (typeof adUnitPath !== 'string') {
            eligible.push(slot);
            continue;
          }
          let excluded = false;
          for (let suffixIndex = 0; suffixIndex < suffixes.length; suffixIndex += 1) {
            if (adUnitPath.endsWith(suffixes[suffixIndex] as string)) {
              excluded = true;
              break;
            }
          }
          if (!excluded) eligible.push(slot);
        }
        return Object.freeze(eligible);
      });
      request.operation = operation;
      if (!request.active) {
        try {
          operation.dispose();
        } catch {
          // The terminal request already resumed GPT.
        }
        return completion;
      }
      void operation.result.then((eligible) => {
        if (
          !request.active ||
          !owner.active ||
          currentNavigation() !== navigation ||
          !navigation.isCurrent()
        ) {
          settle();
          return;
        }
        if (eligible.length === 0) {
          settle();
          return;
        }
        let auction: PrebidRefreshAuctionOperation;
        try {
          auction = options.runSyntheticAuction(Object.freeze([...eligible]), navigation);
        } catch {
          settle();
          return;
        }
        request.auctionOperation = auction;
        if (!request.active) {
          try {
            auction.dispose();
          } catch {
            // The policy's completion latch has already settled.
          }
          return;
        }
        void auction.completion.then(settle, settle);
      }, settle);
    } catch {
      settle();
    }
    return completion;
  };

  return Object.freeze({
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      const snapshot = [...pending];
      for (let index = 0; index < snapshot.length; index += 1) snapshot[index]?.settle();
    },
    prepare,
  });
}

export type PrebidBidPublicationFailureReason =
  | 'descriptor_invalid'
  | 'prebid_admission_failed'
  | 'prebid_contract_violation'
  | 'registry_full'
  | 'reservation_collision'
  | 'winner_not_renderable';

export type PrebidBidPublicationResult =
  | Readonly<{ ok: true; bid: Readonly<PreparedTrustedBidV1> }>
  | Readonly<{ ok: false; reason: PrebidBidPublicationFailureReason }>;

type PrebidPublicationNavigation = NavigationSession;

export interface PrebidBidPublicationInput {
  readonly admitTrustedBid: (preparedBid: Readonly<PreparedTrustedBidV1>) => unknown;
  readonly auctionId: string;
  readonly adUnitCode: string;
  readonly bid: BrowserAuctionBidV1;
  readonly generatedBid: unknown;
  readonly navigation: PrebidPublicationNavigation;
  readonly reservations: Pick<ReservationService, 'registerPrebidLease' | 'tombstonePrebidLease'>;
  readonly trackAdmittedBid: (
    preparedBid: Readonly<PreparedTrustedBidV1>,
    navigation: PrebidPublicationNavigation
  ) => boolean;
}

function isCurrentProjectedWinner(input: PrebidBidPublicationInput): boolean {
  try {
    const projection = input.navigation.currentAuctionProjection as
      Readonly<BrowserAuctionProjectionV1> | undefined;
    if (
      !projection ||
      !objectIsFrozenIntrinsic(projection) ||
      !objectIsFrozenIntrinsic(projection.auction) ||
      !objectIsFrozenIntrinsic(projection.auction.results) ||
      !objectIsFrozenIntrinsic(projection.bids) ||
      !objectIsFrozenIntrinsic(input.bid) ||
      !objectIsFrozenIntrinsic(input.bid.targeting) ||
      !objectIsFrozenIntrinsic(input.bid.renderSource) ||
      !input.navigation.isCurrent() ||
      input.auctionId !== projection.auction.auctionId ||
      input.adUnitCode !== input.bid.slot ||
      !isRendererReservationIdV1(input.bid.rendererReservationId)
    ) {
      return false;
    }

    let bidMatches = 0;
    for (let index = 0; index < projection.bids.length; index += 1) {
      if (projection.bids[index] === input.bid) bidMatches += 1;
    }
    if (bidMatches !== 1) return false;

    let winnerMatches = 0;
    for (let index = 0; index < projection.auction.results.length; index += 1) {
      const result = projection.auction.results[index];
      if (
        result?.outcome === 'winner' &&
        result.slot === input.bid.slot &&
        result.candidateId === input.bid.candidateId
      ) {
        winnerMatches += 1;
      }
    }
    return winnerMatches === 1;
  } catch {
    return false;
  }
}

function prepareTrustedBid(
  input: PrebidBidPublicationInput
): Readonly<PreparedTrustedBidV1> | undefined {
  try {
    const generated = ownDataObject(input.generatedBid);
    const width = input.bid.renderSource.width;
    const height = input.bid.renderSource.height;
    if (
      !generated ||
      !validBoundedString(generated.requestId, 64) ||
      !validBoundedString(generated.adId, 128) ||
      !Object.is(generated.cpm, input.bid.cpm) ||
      generated.width !== width ||
      generated.height !== height ||
      !validDimension(width) ||
      !validDimension(height)
    ) {
      return undefined;
    }

    const advertiserDomains = Object.freeze([] as string[]);
    const meta = Object.freeze({
      advertiserDomains,
      tsAuctionId: input.auctionId,
      tsBidId: input.bid.upstreamBidId,
    });
    const creativeId =
      input.bid.renderSource.type === 'aps' && input.bid.renderSource.creativeId
        ? input.bid.renderSource.creativeId
        : input.bid.upstreamBidId;
    const bid = Object.freeze({
      requestId: generated.requestId,
      adId: input.bid.rendererReservationId,
      cpm: input.bid.cpm,
      width,
      height,
      ad: '' as const,
      ttl: 300 as const,
      creativeId,
      netRevenue: true as const,
      currency: 'USD' as const,
      bidderCode: 'trustedServer' as const,
      meta,
    });
    return Object.freeze({ auctionId: input.auctionId, adUnitCode: input.adUnitCode, bid });
  } catch {
    return undefined;
  }
}

function registrationFailure(reason: string): PrebidBidPublicationFailureReason {
  if (reason === 'reservation_collision') return 'reservation_collision';
  if (reason === 'registry_full') return 'registry_full';
  if (reason === 'stale_owner' || reason === 'service_disposed') {
    return 'winner_not_renderable';
  }
  if (
    reason === 'invalid_reservation_id' ||
    reason === 'invalid_slot' ||
    reason === 'invalid_render_source' ||
    reason === 'invalid_winner_context' ||
    reason === 'prebid_cpm_mismatch'
  ) {
    return 'descriptor_invalid';
  }
  return 'prebid_admission_failed';
}

/** Register before exposing one TS-owned bid through the version-pinned Prebid boundary. */
export function publishPrebidBid(input: PrebidBidPublicationInput): PrebidBidPublicationResult {
  if (!isCurrentProjectedWinner(input)) {
    return Object.freeze({ ok: false, reason: 'winner_not_renderable' });
  }
  const preparedBid = prepareTrustedBid(input);
  if (!preparedBid) return Object.freeze({ ok: false, reason: 'descriptor_invalid' });

  const registration = (() => {
    try {
      return input.reservations.registerPrebidLease({
        reservationId: input.bid.rendererReservationId,
        slot: input.bid.slot,
        navigation: input.navigation,
        auctionId: input.auctionId,
        adUnitCode: input.adUnitCode,
        renderSource: input.bid.renderSource,
        winnerContext: Object.freeze({ selectedCpm: input.bid.cpm }),
        prebidBid: preparedBid.bid,
      });
    } catch {
      return Object.freeze({ ok: false as const, reason: 'service_disposed' as const });
    }
  })();
  if (!registration.ok) {
    return Object.freeze({ ok: false, reason: registrationFailure(registration.reason) });
  }

  let failure: 'prebid_admission_failed' | 'prebid_contract_violation' | undefined;
  try {
    const admission = input.admitTrustedBid(preparedBid);
    if (admission === 'not_admitted') failure = 'prebid_admission_failed';
    else if (admission !== 'admitted') failure = 'prebid_contract_violation';
  } catch (error) {
    failure =
      error instanceof PrebidAdmissionContractError
        ? 'prebid_contract_violation'
        : 'prebid_admission_failed';
  }
  if (!failure) {
    try {
      if (input.trackAdmittedBid(preparedBid, input.navigation)) {
        return Object.freeze({ ok: true, bid: preparedBid });
      }
    } catch {
      // A published bid without selection ownership must stay suppress-only.
    }
    failure = 'prebid_contract_violation';
  }

  const tombstoned = (() => {
    try {
      return input.reservations.tombstonePrebidLease(
        {
          reservationId: input.bid.rendererReservationId,
          auctionId: input.auctionId,
          adUnitCode: input.adUnitCode,
          navigationGeneration: input.navigation.generation,
        },
        failure
      );
    } catch {
      return false;
    }
  })();
  return Object.freeze({
    ok: false,
    reason: tombstoned ? failure : 'prebid_contract_violation',
  });
}

export interface PrebidSelectionCoordinatorOptions {
  readonly activateAttempt: (
    input: Readonly<{
      attempt: RenderAttempt;
      owner: RenderAttemptScope;
      preparedBid: Readonly<PreparedTrustedBidV1>;
    }>
  ) => boolean;
  readonly createAttempt: (owner: RenderAttemptScope) => RenderAttemptCreationResult;
  readonly reservations: Pick<
    ReservationService,
    'promotePrebidSelection' | 'tombstone' | 'tombstonePrebidGroup'
  >;
  readonly scheduler?: RenderScheduler;
}

export interface PrebidSelectionCoordinator {
  readonly track: (
    preparedBid: Readonly<PreparedTrustedBidV1>,
    navigation: NavigationSession
  ) => boolean;
  readonly auctionEnded: (event: unknown, prebid: Readonly<PrebidEventFacade>) => void;
  readonly abort: (navigation: NavigationSession, auctionId: string) => void;
  readonly dispose: () => void;
}

interface TrackedPrebidGroup {
  readonly adUnitCode: string;
  readonly auction: TrackedPrebidAuction;
  readonly bids: Map<string, Readonly<PreparedTrustedBidV1>>;
  active: boolean;
  timer: unknown;
}

interface TrackedPrebidAuction {
  readonly auctionId: string;
  readonly batch: AuctionBatchScope;
  readonly groups: Map<string, TrackedPrebidGroup>;
  readonly navigation: NavigationSession;
  active: boolean;
  promotedAttempts: number;
}

const PREBID_SELECTION_TIMEOUT_MS = 10_000;

function defaultSelectionScheduler(): RenderScheduler {
  return Object.freeze({
    clear: (handle: unknown): void => {
      globalThis.clearTimeout(handle as ReturnType<typeof globalThis.setTimeout>);
    },
    set: (callback: () => void, milliseconds: number): unknown =>
      globalThis.setTimeout(callback, milliseconds),
  });
}

function exactSelectedBid(
  candidate: unknown,
  group: TrackedPrebidGroup
): Readonly<PreparedTrustedBidV1> | undefined {
  const record = ownDataObject(candidate);
  if (
    !record ||
    record.auctionId !== group.auction.auctionId ||
    record.adUnitCode !== group.adUnitCode ||
    typeof record.adId !== 'string'
  ) {
    return undefined;
  }
  const prepared = group.bids.get(record.adId);
  if (!prepared) return undefined;
  const meta = ownDataObject(record.meta);
  return record.requestId === prepared.bid.requestId &&
    Object.is(record.cpm, prepared.bid.cpm) &&
    record.bidderCode === prepared.bid.bidderCode &&
    meta?.tsAuctionId === prepared.auctionId &&
    meta.tsBidId === prepared.bid.meta.tsBidId
    ? prepared
    : undefined;
}

/** Own short Prebid-selection leases without exposing reservation state to the artifact. */
export function createPrebidSelectionCoordinator(
  options: PrebidSelectionCoordinatorOptions
): PrebidSelectionCoordinator {
  const scheduler = options.scheduler ?? defaultSelectionScheduler();
  const auctions: TrackedPrebidAuction[] = [];
  let disposed = false;

  const removeAuction = (auction: TrackedPrebidAuction): void => {
    const index = auctions.indexOf(auction);
    if (index >= 0) auctions.splice(index, 1);
    auction.active = false;
  };

  const clearGroupTimer = (group: TrackedPrebidGroup): void => {
    if (group.timer === undefined) return;
    const timer = group.timer;
    group.timer = undefined;
    try {
      scheduler.clear(timer);
    } catch {
      // Timer cleanup cannot weaken reservation suppression.
    }
  };

  const finishGroup = (
    group: TrackedPrebidGroup,
    state?: 'aborted' | 'prebid_selection_timeout' | 'unselected'
  ): void => {
    if (!group.active) return;
    group.active = false;
    clearGroupTimer(group);
    if (state) {
      try {
        options.reservations.tombstonePrebidGroup(
          {
            auctionId: group.auction.auctionId,
            adUnitCode: group.adUnitCode,
            navigationGeneration: group.auction.navigation.generation,
          },
          state
        );
      } catch {
        // The bounded reservation service remains the suppression authority.
      }
    }
    group.auction.groups.delete(group.adUnitCode);
    if (group.auction.groups.size !== 0) return;
    if (group.auction.promotedAttempts === 0) {
      try {
        group.auction.batch.dispose();
      } catch {
        // Navigation disposal remains the final owner of a hostile batch.
      }
    }
    removeAuction(group.auction);
  };

  const findAuction = (
    navigation: NavigationSession,
    auctionId: string
  ): TrackedPrebidAuction | undefined => {
    for (let index = 0; index < auctions.length; index += 1) {
      const auction = auctions[index];
      if (auction?.active && auction.navigation === navigation && auction.auctionId === auctionId) {
        return auction;
      }
    }
    return undefined;
  };

  const track = (
    preparedBid: Readonly<PreparedTrustedBidV1>,
    navigation: NavigationSession
  ): boolean => {
    try {
      if (
        disposed ||
        !navigation.isCurrent() ||
        !Object.isFrozen(preparedBid) ||
        !Object.isFrozen(preparedBid.bid) ||
        !isRendererReservationIdV1(preparedBid.bid.adId)
      ) {
        return false;
      }
      let auction = findAuction(navigation, preparedBid.auctionId);
      let createdAuction = false;
      if (!auction) {
        const batch = navigation.createAuctionBatch(`prebid:${preparedBid.auctionId}`);
        if (!batch) return false;
        auction = {
          auctionId: preparedBid.auctionId,
          batch,
          groups: new Map(),
          navigation,
          active: true,
          promotedAttempts: 0,
        };
        createdAuction = true;
      }
      let group = auction.groups.get(preparedBid.adUnitCode);
      if (group?.bids.has(preparedBid.bid.adId)) return false;
      if (!group) {
        group = {
          adUnitCode: preparedBid.adUnitCode,
          auction,
          bids: new Map(),
          active: true,
          timer: undefined,
        };
        group.bids.set(preparedBid.bid.adId, preparedBid);
        auction.groups.set(preparedBid.adUnitCode, group);
        if (createdAuction) auctions.push(auction);
        let timer: unknown;
        try {
          timer = scheduler.set(
            () => finishGroup(group as TrackedPrebidGroup, 'prebid_selection_timeout'),
            PREBID_SELECTION_TIMEOUT_MS
          );
          if (!group.active) {
            try {
              scheduler.clear(timer);
            } catch {
              // The synchronously-fired logical deadline remains terminal.
            }
            return false;
          }
          group.timer = timer;
          navigation.onDispose('prebid-selection', () =>
            finishGroup(group as TrackedPrebidGroup, 'aborted')
          );
          if (!group.active || !navigation.isCurrent()) {
            finishGroup(group, 'aborted');
            return false;
          }
        } catch {
          if (timer !== undefined && group.timer === undefined) {
            try {
              scheduler.clear(timer);
            } catch {
              // Failed publication retains no live logical deadline.
            }
          }
          finishGroup(group);
          return false;
        }
        return true;
      }
      group.bids.set(preparedBid.bid.adId, preparedBid);
      return true;
    } catch {
      return false;
    }
  };

  const auctionEnded = (event: unknown, prebid: Readonly<PrebidEventFacade>): void => {
    if (disposed) return;
    const record = ownDataObject(event);
    if (!record || !validBoundedString(record.auctionId, 128)) return;
    const snapshot = auctions.slice();
    for (let auctionIndex = 0; auctionIndex < snapshot.length; auctionIndex += 1) {
      const auction = snapshot[auctionIndex];
      if (!auction?.active || auction.auctionId !== record.auctionId) continue;
      const groups = [...auction.groups.values()];
      for (let groupIndex = 0; groupIndex < groups.length; groupIndex += 1) {
        const group = groups[groupIndex];
        if (!group?.active) continue;
        let highest: readonly object[];
        try {
          highest = prebid.highestBids(group.adUnitCode);
        } catch {
          continue;
        }
        if (highest.length !== 1) {
          finishGroup(group, 'unselected');
          continue;
        }
        const selected: Readonly<PreparedTrustedBidV1>[] = [];
        for (let bidIndex = 0; bidIndex < highest.length; bidIndex += 1) {
          const match = exactSelectedBid(highest[bidIndex], group);
          if (match) selected.push(match);
        }
        if (selected.length !== 1) {
          finishGroup(group, 'unselected');
          continue;
        }
        const prepared = selected[0];
        if (!prepared) {
          finishGroup(group, 'unselected');
          continue;
        }
        const owner = auction.batch.createRenderAttempt(group.adUnitCode);
        if (!owner.ok) {
          finishGroup(group, 'unselected');
          continue;
        }
        let created: RenderAttemptCreationResult;
        try {
          created = options.createAttempt(owner.value);
        } catch {
          owner.value.dispose();
          finishGroup(group, 'unselected');
          continue;
        }
        if (!created.ok) {
          owner.value.dispose();
          finishGroup(group, 'unselected');
          continue;
        }
        let promotion: ReturnType<ReservationService['promotePrebidSelection']>;
        try {
          promotion = options.reservations.promotePrebidSelection({
            reservationId: prepared.bid.adId,
            auctionId: prepared.auctionId,
            adUnitCode: prepared.adUnitCode,
            navigationGeneration: auction.navigation.generation,
            attempt: owner.value,
            prebidBid: prepared.bid,
          });
        } catch {
          created.value.fail('prebid_contract_violation');
          finishGroup(group, 'unselected');
          continue;
        }
        if (!promotion.ok) {
          created.value.fail('prebid_contract_violation');
          finishGroup(group, 'unselected');
          continue;
        }
        let activated: boolean;
        try {
          activated =
            options.activateAttempt(
              Object.freeze({ attempt: created.value, owner: owner.value, preparedBid: prepared })
            ) === true;
        } catch {
          activated = false;
        }
        if (!activated) {
          try {
            options.reservations.tombstone(
              {
                reservationId: prepared.bid.adId,
                slot: prepared.adUnitCode,
                navigationGeneration: auction.navigation.generation,
                attemptId: owner.value.id,
              },
              'stale'
            );
          } catch {
            // A failed PUC activation remains terminal at the attempt boundary.
          }
          created.value.fail('prebid_contract_violation');
          finishGroup(group);
          continue;
        }
        auction.promotedAttempts += 1;
        finishGroup(group);
      }
    }
  };

  const abort = (navigation: NavigationSession, auctionId: string): void => {
    const auction = findAuction(navigation, auctionId);
    if (!auction) return;
    const groups = [...auction.groups.values()];
    for (let index = 0; index < groups.length; index += 1) {
      const group = groups[index];
      if (group) finishGroup(group, 'aborted');
    }
  };

  return Object.freeze({
    track,
    auctionEnded,
    abort,
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      const snapshot = auctions.slice();
      for (let index = 0; index < snapshot.length; index += 1) {
        const auction = snapshot[index];
        if (!auction) continue;
        const groups = [...auction.groups.values()];
        for (let groupIndex = 0; groupIndex < groups.length; groupIndex += 1) {
          const group = groups[groupIndex];
          if (group) finishGroup(group, 'aborted');
        }
        try {
          auction.batch.dispose();
        } catch {
          // Runtime disposal remains terminal under hostile callbacks.
        }
      }
      auctions.length = 0;
    },
  });
}
