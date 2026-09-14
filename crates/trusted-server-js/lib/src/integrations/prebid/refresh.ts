import {
  ownDataArray,
  ownDataObject,
  validBoundedString,
} from '../../core/contracts/auction_projection';
import type { NavigationSession } from '../../kernel/sessions';

interface DeferredOperation<T> {
  readonly result: Promise<T>;
  readonly dispose: () => void;
}

interface DeferredGoogletagFacade {
  readonly adUnitPath?: (slot: object) => unknown;
  readonly clearTargeting: (slot: object, key?: string) => unknown;
}

export interface DeferredGoogletagRunner {
  readonly run: <T>(command: (googletag: DeferredGoogletagFacade) => T) => DeferredOperation<T>;
}

interface DeferredPrebidFacade {
  readonly requestBids: (request: Readonly<Record<string, unknown>>) => unknown;
  readonly setTargetingForGpt: (adUnitCodes: readonly string[]) => unknown;
}

export interface DeferredPrebidRunner {
  readonly run: <T>(
    command: (prebid: DeferredPrebidFacade) => T,
    options?: Readonly<{ signal?: AbortSignal }>
  ) => DeferredOperation<T>;
}

interface DeferredPublisherRefreshCall {
  readonly slots: readonly object[];
}

interface RefreshScheduler {
  readonly clear: (handle: unknown) => void;
  readonly set: (callback: () => void, milliseconds: number) => unknown;
}

const TRUSTED_SERVER_PREBID_BIDDER = 'trustedServer';
const MAX_CONFIG_MEMBERS = 256;
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
  readonly googletag: DeferredGoogletagRunner;
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
  readonly prebid: DeferredPrebidRunner;
  readonly prepareAuction: (slots: readonly object[], navigation: NavigationSession) => unknown;
  readonly scheduler?: RefreshScheduler;
}

export type PrebidSyntheticRefreshRunner = (
  slots: readonly object[],
  navigation: NavigationSession
) => PrebidRefreshAuctionOperation;

export interface PrebidRefreshPolicy {
  readonly dispose: () => void;
  readonly prepare: (
    call: Readonly<DeferredPublisherRefreshCall>
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
  operation: DeferredOperation<readonly object[]> | undefined;
  readonly resolve: () => void;
  readonly settle: () => void;
}

function defaultRefreshScheduler(): RefreshScheduler {
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

/** Rebuild synthetic Prebid units from detached runtime-owned registrations. */
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
    let adapterOperation: DeferredOperation<unknown> | undefined;
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
    call: Readonly<DeferredPublisherRefreshCall>
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
