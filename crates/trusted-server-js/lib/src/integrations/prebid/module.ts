import {
  isRendererReservationIdV1,
  ownDataObject,
  validBoundedString,
  validDimension,
} from '../../core/contracts/auction_projection';
import type { BrowserAuctionBidV1, BrowserAuctionProjectionV1 } from '../../core/types';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';
import type { NavigationSession } from '../../kernel/sessions';
import type { ReservationService } from '../../services/reservations';

export const PREBID_INTEGRATION_ID = 'prebid' as const;

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

/** Exact TS-owned bid passed to the version-pinned Prebid admission boundary. */
export interface PreparedTrustedBidV1 {
  readonly auctionId: string;
  readonly adUnitCode: string;
  readonly bid: Readonly<{
    readonly requestId: string;
    readonly adId: string;
    readonly cpm: number;
    readonly width: number;
    readonly height: number;
    readonly ad: '';
    readonly ttl: 300;
    readonly creativeId: string;
    readonly netRevenue: true;
    readonly currency: 'USD';
    readonly bidderCode: 'trustedServer';
    readonly meta: Readonly<{
      readonly advertiserDomains: readonly string[];
      readonly tsAuctionId: string;
      readonly tsBidId: string;
      readonly tsAdmHash?: string;
    }>;
  }>;
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

type PrebidPublicationNavigation = Pick<
  NavigationSession,
  'currentAuctionProjection' | 'generation' | 'isCurrent' | 'onDispose'
>;

export interface PrebidBidPublicationInput {
  readonly admitTrustedBid: (preparedBid: Readonly<PreparedTrustedBidV1>) => unknown;
  readonly auctionId: string;
  readonly adUnitCode: string;
  readonly bid: BrowserAuctionBidV1;
  readonly generatedBid: unknown;
  readonly navigation: PrebidPublicationNavigation;
  readonly reservations: Pick<ReservationService, 'registerPrebidLease' | 'tombstonePrebidLease'>;
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
  } catch {
    failure = 'prebid_admission_failed';
  }
  if (!failure) return Object.freeze({ ok: true, bid: preparedBid });

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
