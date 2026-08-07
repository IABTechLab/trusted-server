import { parseCacheFetchPolicyV1 } from '../config';
import type {
  AdmRenderSourceV1,
  AuctionDecisionSetV1,
  AuctionSlotFailureReason,
  BidRenderSourceV1,
  BrowserAuctionBidV1,
  BrowserAuctionProjectionV1,
  CacheFetchPolicyV1,
  CacheRenderSourceV1,
  SlotAuctionDecisionV1,
} from '../types';

import { validateApsRenderer } from './aps_renderer';

export const MAX_BROWSER_AUCTION_PROJECTION_BYTES = 8 * 1024 * 1024;

export const MAX_AUCTION_RESULTS = 256;
const MAX_TARGETING_ENTRIES = 32;
const MAX_ADM_BYTES = 512 * 1024;
const MAX_URL_BYTES = 4096;
const textEncoder = new TextEncoder();
const candidateIdPattern = /^[A-Za-z0-9_-]{12}$/;
const reservationIdPattern = /^r1_[A-Za-z0-9_-]{22}$/;
const auctionIdPattern = /^[A-Za-z0-9._:-]{1,128}$/;
const providerPattern = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const targetingKeyPattern = /^[A-Za-z0-9_]{1,20}$/;
const cacheIdPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const auctionFailureReasons = new Set<AuctionSlotFailureReason>([
  'auction_disabled',
  'consent_denied',
  'slot_not_eligible',
  'provider_timeout',
  'provider_error',
  'invalid_provider_response',
  'mediation_failed',
  'winner_not_renderable',
  'identity_generation_failed',
  'internal_error',
]);

export function ownDataObject(
  value: unknown,
  expectedKeys?: readonly string[]
): Record<string, unknown> | undefined {
  try {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
    if (Object.getPrototypeOf(value) !== Object.prototype) return undefined;
    if (Object.getOwnPropertySymbols(value).length !== 0) return undefined;
    const names = Object.getOwnPropertyNames(value);
    if (
      expectedKeys &&
      (names.length !== expectedKeys.length || expectedKeys.some((key) => !names.includes(key)))
    ) {
      return undefined;
    }
    const snapshot: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (const name of names) {
      const descriptor = Object.getOwnPropertyDescriptor(value, name);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      snapshot[name] = descriptor.value;
    }
    return snapshot;
  } catch {
    return undefined;
  }
}

export function ownDataArray(value: unknown, maximum: number): unknown[] | undefined {
  try {
    if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype) return undefined;
    if (value.length > maximum || Object.getOwnPropertySymbols(value).length !== 0)
      return undefined;
    const names = Object.getOwnPropertyNames(value);
    if (names.length !== value.length + 1 || !names.includes('length')) return undefined;
    const snapshot: unknown[] = [];
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      snapshot.push(descriptor.value);
    }
    return snapshot;
  } catch {
    return undefined;
  }
}

function validUnicodeScalars(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return false;
    }
  }
  return true;
}

function hasAsciiControl(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return true;
  }
  return false;
}

export function validBoundedString(
  value: unknown,
  maximumBytes: number,
  options: { allowControls?: boolean; maximumScalars?: number } = {}
): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    validUnicodeScalars(value) &&
    (options.allowControls === true || !hasAsciiControl(value)) &&
    textEncoder.encode(value).length <= maximumBytes &&
    (options.maximumScalars === undefined || Array.from(value).length <= options.maximumScalars)
  );
}

export function validDimension(value: unknown): value is number {
  return (
    typeof value === 'number' &&
    Number.isFinite(value) &&
    Number.isInteger(value) &&
    value >= 1 &&
    value <= 4096
  );
}

export function isAuctionCandidateIdV1(value: unknown): value is string {
  return typeof value === 'string' && candidateIdPattern.test(value);
}

export function isAuctionProviderIdV1(value: unknown): value is string {
  return typeof value === 'string' && providerPattern.test(value);
}

export function jsonUtf8ByteLength(value: unknown): number {
  return textEncoder.encode(JSON.stringify(value)).length;
}

/** Whether a value is one exact server-minted renderer reservation identity. */
export function isRendererReservationIdV1(value: unknown): value is string {
  return typeof value === 'string' && reservationIdPattern.test(value);
}

/** Validate and copy one exact browser render-source contract. */
export function parseBidRenderSourceV1(
  value: unknown,
  cachePolicy?: Readonly<CacheFetchPolicyV1>
): BidRenderSourceV1 | undefined {
  const record = ownDataObject(value);
  if (!record || typeof record.type !== 'string') return undefined;

  if (record.type === 'aps') {
    const keys = [
      'type',
      'version',
      'accountId',
      'bidId',
      ...(Object.prototype.hasOwnProperty.call(record, 'creativeId') ? ['creativeId'] : []),
      'tagType',
      'creativeUrl',
      'aaxResponse',
      'width',
      'height',
    ];
    if (!ownDataObject(value, keys)) return undefined;
    const renderer = validateApsRenderer(record);
    if (!renderer) return undefined;
    return {
      type: 'aps',
      version: 1,
      accountId: renderer.accountId,
      bidId: renderer.bidId,
      ...(renderer.creativeId === undefined ? {} : { creativeId: renderer.creativeId }),
      tagType: renderer.tagType,
      creativeUrl: renderer.creativeUrl,
      aaxResponse: renderer.aaxResponse,
      width: renderer.width,
      height: renderer.height,
    };
  }

  if (record.type === 'adm') {
    const source = ownDataObject(value, ['type', 'version', 'adm', 'width', 'height']);
    if (
      !source ||
      source.version !== 1 ||
      !validBoundedString(source.adm, MAX_ADM_BYTES, { allowControls: true }) ||
      !validDimension(source.width) ||
      !validDimension(source.height)
    ) {
      return undefined;
    }
    return {
      type: 'adm',
      version: 1,
      adm: source.adm,
      width: source.width,
      height: source.height,
    } satisfies AdmRenderSourceV1;
  }

  if (record.type === 'cache') {
    const source = ownDataObject(value, [
      'type',
      'version',
      'cacheId',
      'fetchUrl',
      'width',
      'height',
    ]);
    if (
      !source ||
      source.version !== 1 ||
      typeof source.cacheId !== 'string' ||
      !cacheIdPattern.test(source.cacheId) ||
      !validBoundedString(source.fetchUrl, MAX_URL_BYTES) ||
      !validDimension(source.width) ||
      !validDimension(source.height) ||
      !cachePolicy
    ) {
      return undefined;
    }
    let fetchUrl: URL;
    try {
      fetchUrl = new URL(source.fetchUrl);
    } catch {
      return undefined;
    }
    if (
      fetchUrl.protocol !== 'https:' ||
      fetchUrl.username !== '' ||
      fetchUrl.password !== '' ||
      fetchUrl.hash !== '' ||
      [...fetchUrl.searchParams.keys()].length !== 1 ||
      fetchUrl.searchParams.get('uuid') !== source.cacheId ||
      fetchUrl.search !== `?uuid=${encodeURIComponent(source.cacheId)}`
    ) {
      return undefined;
    }
    let policyBase: URL;
    try {
      policyBase = new URL(cachePolicy.baseUrl);
    } catch {
      return undefined;
    }
    const expected = new URL(policyBase.href);
    expected.search = `?uuid=${encodeURIComponent(source.cacheId)}`;
    if (
      fetchUrl.origin !== policyBase.origin ||
      fetchUrl.port !== policyBase.port ||
      fetchUrl.pathname !== policyBase.pathname ||
      fetchUrl.href !== expected.href
    ) {
      return undefined;
    }
    return {
      type: 'cache',
      version: 1,
      cacheId: source.cacheId,
      fetchUrl: fetchUrl.href,
      width: source.width,
      height: source.height,
    } satisfies CacheRenderSourceV1;
  }

  return undefined;
}

/** Validate and copy one exact auction decision-set contract. */
export function parseAuctionDecisionSetV1(value: unknown): AuctionDecisionSetV1 | undefined {
  const record = ownDataObject(value, ['version', 'auctionId', 'results']);
  if (!record || record.version !== 1 || typeof record.auctionId !== 'string') return undefined;
  if (!auctionIdPattern.test(record.auctionId)) return undefined;
  const results = ownDataArray(record.results, MAX_AUCTION_RESULTS);
  if (!results) return undefined;

  const parsed: SlotAuctionDecisionV1[] = [];
  const slots = new Set<string>();
  const candidates = new Set<string>();
  for (const raw of results) {
    const base = ownDataObject(raw);
    if (!base || !validBoundedString(base.slot, 256) || slots.has(base.slot)) return undefined;
    slots.add(base.slot);
    if (base.outcome === 'winner') {
      const winner = ownDataObject(raw, ['slot', 'outcome', 'candidateId']);
      if (
        !winner ||
        !isAuctionCandidateIdV1(winner.candidateId) ||
        candidates.has(winner.candidateId)
      ) {
        return undefined;
      }
      candidates.add(winner.candidateId);
      parsed.push({ slot: base.slot, outcome: 'winner', candidateId: winner.candidateId });
    } else if (base.outcome === 'no_bid') {
      if (!ownDataObject(raw, ['slot', 'outcome'])) return undefined;
      parsed.push({ slot: base.slot, outcome: 'no_bid' });
    } else if (base.outcome === 'failed') {
      const failed = ownDataObject(raw, ['slot', 'outcome', 'reason']);
      if (
        !failed ||
        typeof failed.reason !== 'string' ||
        !auctionFailureReasons.has(failed.reason as AuctionSlotFailureReason)
      ) {
        return undefined;
      }
      parsed.push({
        slot: base.slot,
        outcome: 'failed',
        reason: failed.reason as AuctionSlotFailureReason,
      });
    } else {
      return undefined;
    }
  }

  return { version: 1, auctionId: record.auctionId, results: parsed };
}

function parseTargeting(value: unknown): Record<string, string> | undefined {
  const record = ownDataObject(value);
  if (!record) return undefined;
  const entries = Object.entries(record);
  if (entries.length > MAX_TARGETING_ENTRIES) return undefined;
  const targeting: Record<string, string> = {};
  for (const [key, entry] of entries.sort(([left], [right]) =>
    left < right ? -1 : left > right ? 1 : 0
  )) {
    if (
      key === 'hb_adid' ||
      !targetingKeyPattern.test(key) ||
      !validBoundedString(entry, 160, { maximumScalars: 40 })
    ) {
      return undefined;
    }
    Object.defineProperty(targeting, key, {
      value: entry,
      enumerable: true,
      writable: true,
      configurable: true,
    });
  }
  return targeting;
}

function parseBrowserBid(
  value: unknown,
  cachePolicy?: Readonly<CacheFetchPolicyV1>
): BrowserAuctionBidV1 | undefined {
  const bid = ownDataObject(value, [
    'candidateId',
    'slot',
    'provider',
    'upstreamBidId',
    'cpm',
    'currency',
    'targeting',
    'rendererReservationId',
    'renderSource',
  ]);
  if (
    !bid ||
    !isAuctionCandidateIdV1(bid.candidateId) ||
    !validBoundedString(bid.slot, 256) ||
    !isAuctionProviderIdV1(bid.provider) ||
    !validBoundedString(bid.upstreamBidId, 64) ||
    typeof bid.cpm !== 'number' ||
    !Number.isFinite(bid.cpm) ||
    bid.cpm < 0 ||
    bid.currency !== 'USD' ||
    !isRendererReservationIdV1(bid.rendererReservationId)
  ) {
    return undefined;
  }
  const targeting = parseTargeting(bid.targeting);
  const renderSource = parseBidRenderSourceV1(bid.renderSource, cachePolicy);
  if (!targeting || !renderSource) return undefined;
  return {
    candidateId: bid.candidateId,
    slot: bid.slot,
    provider: bid.provider,
    upstreamBidId: bid.upstreamBidId,
    cpm: bid.cpm,
    currency: 'USD',
    targeting,
    rendererReservationId: bid.rendererReservationId,
    renderSource,
  };
}

/** Validate, canonicalize, and deep-copy a complete browser auction projection. */
export function parseBrowserAuctionProjectionV1(
  value: unknown,
  cachePolicyValue?: unknown
): BrowserAuctionProjectionV1 | undefined {
  try {
    const cachePolicy =
      cachePolicyValue === undefined ? undefined : parseCacheFetchPolicyV1(cachePolicyValue);
    if (cachePolicyValue !== undefined && !cachePolicy) return undefined;
    const record = ownDataObject(value, ['version', 'auction', 'bids']);
    if (!record || record.version !== 1) return undefined;
    const auction = parseAuctionDecisionSetV1(record.auction);
    const rawBids = ownDataArray(record.bids, MAX_AUCTION_RESULTS);
    if (!auction || !rawBids) return undefined;
    const bids: BrowserAuctionBidV1[] = [];
    const candidateIds = new Set<string>();
    const reservationIds = new Set<string>();
    for (const raw of rawBids) {
      const bid = parseBrowserBid(raw, cachePolicy);
      if (
        !bid ||
        candidateIds.has(bid.candidateId) ||
        reservationIds.has(bid.rendererReservationId)
      ) {
        return undefined;
      }
      candidateIds.add(bid.candidateId);
      reservationIds.add(bid.rendererReservationId);
      bids.push(bid);
    }

    const winners = auction.results.filter(
      (result): result is Extract<SlotAuctionDecisionV1, { outcome: 'winner' }> =>
        result.outcome === 'winner'
    );
    if (
      winners.length !== bids.length ||
      winners.some(
        (winner, index) =>
          bids[index]?.candidateId !== winner.candidateId || bids[index]?.slot !== winner.slot
      )
    ) {
      return undefined;
    }

    const projection: BrowserAuctionProjectionV1 = { version: 1, auction, bids };
    if (jsonUtf8ByteLength(projection) > MAX_BROWSER_AUCTION_PROJECTION_BYTES) {
      return undefined;
    }
    return projection;
  } catch {
    return undefined;
  }
}
