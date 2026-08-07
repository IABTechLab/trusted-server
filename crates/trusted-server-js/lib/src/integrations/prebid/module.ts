import {
  isRendererReservationIdV1,
  ownDataObject,
  validBoundedString,
  validDimension,
} from '../../core/contracts/auction_projection';
import type { BrowserAuctionBidV1, BrowserAuctionProjectionV1 } from '../../core/types';
import {
  PrebidAdmissionContractError,
  type PrebidEventFacade,
  type PreparedTrustedBidV1,
} from '../../adapters/prebid';
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
      Reflect.ownKeys(candidate).length !== 1
    ) {
      return undefined;
    }
    const start = objectGetOwnPropertyDescriptorIntrinsic(candidate, 'start');
    if (!start || !('value' in start) || typeof start.value !== 'function') return undefined;
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
        activate: ({ afterCommit }: IntegrationActivationContext) => {
          afterCommit(() => runtime.start(config));
        },
      });
    },
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
