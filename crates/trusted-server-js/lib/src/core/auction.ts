// Shared auction module: builds AdRequest payloads, sends them to /auction,
// and parses OpenRTB seatbid responses. Used by both the core requestAds flow
// and the Prebid.js trustedServer adapter.

import { parseApsRendererDescriptor, validateApsRenderer } from '../integrations/aps/render';

import { parseCacheFetchPolicyV1 } from './config';
import { log } from './log';
import type {
  AdmRenderSourceV1,
  ApsRendererV1,
  AuctionDecisionSetV1,
  AuctionSlotFailureReason,
  BidRenderSourceV1,
  BrowserAuctionBidV1,
  BrowserAuctionProjectionV1,
  CacheFetchPolicyV1,
  CacheRenderSourceV1,
  SlotAuctionDecisionV1,
} from './types';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** A single ad unit in the AdRequest payload sent to POST /auction. */
export interface AdRequestUnit {
  code: string;
  mediaTypes: {
    banner?: { sizes: number[][] };
  };
  bids: Array<{ bidder: string; params: Record<string, unknown> }>;
}

/** A user identifier within an auction-level EID entry. */
export interface AuctionUid {
  id: string;
  atype?: number;
  ext?: Record<string, unknown>;
}

/** An auction-level EID entry forwarded to the server. */
export interface AuctionEid {
  source: string;
  uids: AuctionUid[];
}

/** The payload POSTed to the /auction orchestrator. */
export interface AdRequest {
  adUnits: AdRequestUnit[];
  config?: Record<string, unknown>;
  eids?: AuctionEid[];
}

/** A parsed bid from an OpenRTB seatbid response. */
export interface AuctionBid {
  /** Matches the `impid` in the response — corresponds to adUnit `code`. */
  impid: string;
  /** Creative HTML (already rewritten with proxy URLs by the server). */
  adm?: string | undefined;
  /** Typed APS renderer descriptor, when the bid does not carry `adm`. */
  renderer?: ApsRendererV1 | undefined;
  /** CPM price. */
  price: number;
  /** Creative width. */
  width: number;
  /** Creative height. */
  height: number;
  /** Seat / bidder code from the seatbid. */
  seat: string;
  /** Creative ID. */
  creativeId: string;
  /** Advertiser domains. */
  adomain: string[];
  /** Server-side auction ID used for render tracing. */
  auctionId?: string | undefined;
  /** Upstream OpenRTB bid ID used for render tracing. */
  bidId?: string | undefined;
  /** Trace hash of the delivered creative markup. */
  admHash?: string | undefined;
}

export interface TrustedServerAuctionBidV1 {
  candidateId: string;
  rendererReservationId: string;
  impid: string;
  provider: string;
  price: number;
  width: number;
  height: number;
  renderSource: BidRenderSourceV1;
  adm?: string | undefined;
}

export interface TrustedServerAuctionResponseV1 {
  auction: AuctionDecisionSetV1;
  bids: TrustedServerAuctionBidV1[];
}

export const MAX_BROWSER_AUCTION_PROJECTION_BYTES = 8 * 1024 * 1024;

const MAX_AUCTION_RESULTS = 256;
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

/** Whether a value is one exact server-minted renderer reservation identity. */
export function isRendererReservationIdV1(value: unknown): value is string {
  return typeof value === 'string' && reservationIdPattern.test(value);
}

function ownDataObject(
  value: unknown,
  expectedKeys?: readonly string[]
): Record<string, unknown> | undefined {
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
  for (const name of names) {
    const descriptor = Object.getOwnPropertyDescriptor(value, name);
    if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
  }
  return value as Record<string, unknown>;
}

function ownDataArray(value: unknown, maximum: number): unknown[] | undefined {
  if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype) return undefined;
  if (value.length > maximum || Object.getOwnPropertySymbols(value).length !== 0) return undefined;
  const names = Object.getOwnPropertyNames(value);
  if (names.length !== value.length + 1 || !names.includes('length')) return undefined;
  for (let index = 0; index < value.length; index += 1) {
    const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
    if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
  }
  return value;
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

function validBoundedString(
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

function validDimension(value: unknown): value is number {
  return (
    typeof value === 'number' &&
    Number.isFinite(value) &&
    Number.isInteger(value) &&
    value >= 1 &&
    value <= 4096
  );
}

function parseRenderSource(
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
    const renderer = validateApsRenderer(value);
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

function parseDecisionSet(value: unknown): AuctionDecisionSetV1 | undefined {
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
        typeof winner.candidateId !== 'string' ||
        !candidateIdPattern.test(winner.candidateId) ||
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
    typeof bid.candidateId !== 'string' ||
    !candidateIdPattern.test(bid.candidateId) ||
    !validBoundedString(bid.slot, 256) ||
    typeof bid.provider !== 'string' ||
    !providerPattern.test(bid.provider) ||
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
  const renderSource = parseRenderSource(bid.renderSource, cachePolicy);
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
  const cachePolicy =
    cachePolicyValue === undefined ? undefined : parseCacheFetchPolicyV1(cachePolicyValue);
  if (cachePolicyValue !== undefined && !cachePolicy) return undefined;
  const record = ownDataObject(value, ['version', 'auction', 'bids']);
  if (!record || record.version !== 1) return undefined;
  const auction = parseDecisionSet(record.auction);
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
  if (
    textEncoder.encode(JSON.stringify(projection)).length > MAX_BROWSER_AUCTION_PROJECTION_BYTES
  ) {
    return undefined;
  }
  return projection;
}

/** Parse the coordinated-cutover `/auction` wire without activating it in production yet. */
export function parseTrustedServerAuctionResponseV1(
  value: unknown,
  cachePolicyValue?: unknown
): TrustedServerAuctionResponseV1 | undefined {
  const cachePolicy =
    cachePolicyValue === undefined ? undefined : parseCacheFetchPolicyV1(cachePolicyValue);
  if (cachePolicyValue !== undefined && !cachePolicy) return undefined;
  const body = ownDataObject(value, ['id', 'seatbid', 'cur', 'ext']);
  if (!body || typeof body.id !== 'string' || body.cur !== 'USD') return undefined;
  const responseExt = ownDataObject(body.ext, ['trusted_server']);
  const trustedResponseExt = ownDataObject(responseExt?.trusted_server, ['slot_results']);
  const auction = parseDecisionSet(trustedResponseExt?.slot_results);
  const seatbids = ownDataArray(body.seatbid, MAX_AUCTION_RESULTS);
  if (!auction || body.id !== auction.auctionId || !seatbids) return undefined;

  const bids: TrustedServerAuctionBidV1[] = [];
  for (const rawSeat of seatbids) {
    const seat = ownDataObject(rawSeat, ['seat', 'bid']);
    if (!seat || typeof seat.seat !== 'string' || !providerPattern.test(seat.seat))
      return undefined;
    const rawBids = ownDataArray(seat.bid, MAX_AUCTION_RESULTS - bids.length);
    if (!rawBids || rawBids.length === 0) return undefined;
    for (const rawBid of rawBids) {
      const rawBidRecord = ownDataObject(rawBid);
      if (!rawBidRecord) return undefined;
      const bid = ownDataObject(rawBid, [
        'id',
        'impid',
        'price',
        ...(Object.prototype.hasOwnProperty.call(rawBidRecord, 'adm') ? ['adm'] : []),
        'w',
        'h',
        'ext',
      ]);
      const extension = ownDataObject(bid?.ext, ['trusted_server']);
      const trusted = ownDataObject(extension?.trusted_server, [
        'candidate_id',
        'slot_id',
        'render_source',
      ]);
      if (
        !bid ||
        !trusted ||
        !isRendererReservationIdV1(bid.id) ||
        !validBoundedString(bid.impid, 256) ||
        typeof trusted.candidate_id !== 'string' ||
        !candidateIdPattern.test(trusted.candidate_id) ||
        trusted.slot_id !== bid.impid ||
        typeof bid.price !== 'number' ||
        !Number.isFinite(bid.price) ||
        bid.price < 0 ||
        !validDimension(bid.w) ||
        !validDimension(bid.h)
      ) {
        return undefined;
      }
      const renderSource = parseRenderSource(trusted.render_source, cachePolicy);
      if (!renderSource || renderSource.width !== bid.w || renderSource.height !== bid.h) {
        return undefined;
      }
      if (
        (renderSource.type === 'adm' &&
          Object.prototype.hasOwnProperty.call(bid, 'adm') &&
          bid.adm !== renderSource.adm) ||
        (renderSource.type !== 'adm' && Object.prototype.hasOwnProperty.call(bid, 'adm'))
      ) {
        return undefined;
      }
      bids.push({
        candidateId: trusted.candidate_id,
        rendererReservationId: bid.id,
        impid: bid.impid,
        provider: seat.seat,
        price: bid.price,
        width: bid.w,
        height: bid.h,
        renderSource,
        ...(renderSource.type === 'adm' ? { adm: renderSource.adm } : {}),
      });
    }
  }

  const winners = auction.results.filter(
    (result): result is Extract<SlotAuctionDecisionV1, { outcome: 'winner' }> =>
      result.outcome === 'winner'
  );
  const candidates = new Set<string>();
  const reservations = new Set<string>();
  if (
    winners.length !== bids.length ||
    bids.some((bid) => {
      if (candidates.has(bid.candidateId) || reservations.has(bid.rendererReservationId)) {
        return true;
      }
      candidates.add(bid.candidateId);
      reservations.add(bid.rendererReservationId);
      const winner = winners.find((entry) => entry.candidateId === bid.candidateId);
      return !winner || winner.slot !== bid.impid;
    }) ||
    winners.some((winner) => !bids.some((bid) => bid.candidateId === winner.candidateId)) ||
    textEncoder.encode(JSON.stringify(value)).length > MAX_BROWSER_AUCTION_PROJECTION_BYTES
  ) {
    return undefined;
  }

  const bidsByCandidate = new Map(bids.map((bid) => [bid.candidateId, bid]));
  const orderedBids: TrustedServerAuctionBidV1[] = [];
  for (const winner of winners) {
    const bid = bidsByCandidate.get(winner.candidateId);
    if (!bid) return undefined;
    orderedBids.push(bid);
  }

  return { auction, bids: orderedBids };
}

// ---------------------------------------------------------------------------
// AdRequest building
// ---------------------------------------------------------------------------

/**
 * Build an {@link AdRequest} from an array of ad-unit-like objects.
 *
 * Accepts both plain tsjs `AdUnit` objects and Prebid-style `BidRequest`
 * objects (which carry `adUnitCode` instead of `code`).
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function buildAdRequest(units: any[], options?: { eids?: AuctionEid[] }): AdRequest {
  const unitMap = new Map<string, AdRequestUnit>();

  for (const unit of units) {
    const code: string = unit.adUnitCode ?? unit.code ?? '';
    if (!unitMap.has(code)) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const mediaTypes: any = {};
      if (unit.mediaTypes?.banner) {
        mediaTypes.banner = {
          sizes: unit.mediaTypes.banner.sizes ?? unit.sizes ?? [],
        };
      }
      unitMap.set(code, { code, mediaTypes, bids: [] });
    }

    // If the source object carries a `bidder` field (Prebid BidRequest style),
    // add it as a bid entry. Otherwise copy the existing `bids` array.
    if (unit.bidder) {
      unitMap.get(code)!.bids.push({
        bidder: unit.bidder,
        params: unit.params ?? {},
      });
    } else if (Array.isArray(unit.bids)) {
      for (const bid of unit.bids) {
        unitMap.get(code)!.bids.push({
          bidder: bid.bidder ?? '',
          params: bid.params ?? {},
        });
      }
    }
  }

  const request: AdRequest = { adUnits: [...unitMap.values()] };
  if (options?.eids && options.eids.length > 0) {
    request.eids = options.eids;
  }
  return request;
}

// ---------------------------------------------------------------------------
// OpenRTB response parsing
// ---------------------------------------------------------------------------

/**
 * Parse an OpenRTB-style response body into a flat array of {@link AuctionBid}.
 *
 * Parsing the renderer here is intentionally structural. The exact decoded
 * APS envelope is validated immediately before any DOM or message side effect.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function parseAuctionResponse(body: any): AuctionBid[] {
  const bids: AuctionBid[] = [];
  const seatbids = body?.seatbid;
  if (!Array.isArray(seatbids)) return bids;
  const responseAuctionId = typeof body?.id === 'string' && body.id !== '' ? body.id : undefined;

  for (const seatbid of seatbids) {
    const seat: string = typeof seatbid?.seat === 'string' ? seatbid.seat : 'unknown';
    const seatBids = seatbid?.bid;
    if (!Array.isArray(seatBids)) continue;

    for (const bid of seatBids) {
      const impid = typeof bid?.impid === 'string' ? bid.impid : '';
      const renderer = parseApsRendererDescriptor(bid?.ext?.trusted_server?.renderer);
      const width = typeof bid?.w === 'number' ? bid.w : (renderer?.width ?? 300);
      const height = typeof bid?.h === 'number' ? bid.h : (renderer?.height ?? 250);
      const creativeId =
        typeof bid?.crid === 'string' ? bid.crid : (renderer?.creativeId ?? `${seat}-${impid}`);
      const tsExt = bid?.ext?.ts;

      bids.push({
        impid,
        // Preserve non-string untrusted values so the render-time sanitizer
        // rejects them explicitly instead of silently converting them to an
        // empty no-op creative.
        adm: bid?.adm ?? '',
        ...(renderer ? { renderer } : {}),
        price: typeof bid?.price === 'number' ? bid.price : 0,
        width,
        height,
        seat,
        creativeId,
        adomain: Array.isArray(bid?.adomain)
          ? bid.adomain.filter((domain: unknown): domain is string => typeof domain === 'string')
          : [],
        auctionId:
          typeof tsExt?.auction_id === 'string' && tsExt.auction_id !== ''
            ? tsExt.auction_id
            : responseAuctionId,
        bidId: typeof bid?.id === 'string' && bid.id !== '' ? bid.id : undefined,
        admHash:
          typeof tsExt?.adm_hash === 'string' && tsExt.adm_hash !== '' ? tsExt.adm_hash : undefined,
      });
    }
  }
  return bids;
}

// ---------------------------------------------------------------------------
// Auction HTTP call
// ---------------------------------------------------------------------------

/**
 * POST an {@link AdRequest} to the given endpoint and return parsed bids.
 *
 * Returns an empty array on network or parse errors (non-throwing).
 */
export async function sendAuction(endpoint: string, request: AdRequest): Promise<AuctionBid[]> {
  if (typeof fetch !== 'function') {
    log.warn('auction: fetch not available');
    return [];
  }

  log.info('auction: sending request', { endpoint, units: request.adUnits.length });

  try {
    const response = await fetch(endpoint, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      credentials: 'same-origin',
      body: JSON.stringify(request),
      keepalive: true,
    });

    const contentType = response.headers.get('content-type') || '';
    if (response.ok && contentType.includes('application/json')) {
      const data: unknown = await response.json();
      const bids = parseAuctionResponse(data);
      log.info('auction: received bids', { count: bids.length });
      return bids;
    }

    log.warn('auction: unexpected response', {
      ok: response.ok,
      status: response.status,
      ct: contentType,
    });
    return [];
  } catch (error) {
    log.warn('auction: request failed', error);
    return [];
  }
}
