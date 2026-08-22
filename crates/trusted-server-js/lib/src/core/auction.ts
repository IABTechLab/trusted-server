// Shared auction module: builds AdRequest payloads, sends them to /auction,
// and parses OpenRTB seatbid responses. Used by both the core requestAds flow
// and the Prebid.js trustedServer adapter.

import { parseApsRendererDescriptor } from './contracts/aps_renderer';
import {
  MAX_AUCTION_RESULTS,
  MAX_BROWSER_AUCTION_PROJECTION_BYTES,
  isAuctionCandidateIdV1,
  isAuctionProviderIdV1,
  isRendererReservationIdV1,
  jsonUtf8ByteLength,
  ownDataArray,
  ownDataObject,
  parseAuctionDecisionSetV1 as parseDecisionSet,
  parseBidRenderSourceV1 as parseRenderSource,
  validBoundedString,
  validDimension,
} from './contracts/auction_projection';
import { log } from './log';
import type {
  ApsRendererV1,
  AuctionDecisionSetV1,
  BidRenderSourceV1,
  BrowserAuctionProjectionV1,
  SlotAuctionDecisionV1,
} from './types';

export {
  MAX_BROWSER_AUCTION_PROJECTION_BYTES,
  isRendererReservationIdV1,
  parseBrowserAuctionProjectionV1,
} from './contracts/auction_projection';

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

interface TrustedServerAuctionBidBaseV1 {
  candidateId: string;
  impid: string;
  provider: string;
  price: number;
  width: number;
  height: number;
}

export type TrustedServerAuctionBidV1 =
  | (TrustedServerAuctionBidBaseV1 & {
      rendererReservationId: string;
      renderSource: Exclude<BidRenderSourceV1, { type: 'pbs_cache' }>;
      adm?: string | undefined;
    })
  | (TrustedServerAuctionBidBaseV1 & {
      renderSource: Extract<BidRenderSourceV1, { type: 'pbs_cache' }>;
    });

export interface TrustedServerAuctionResponseV1 {
  auction: AuctionDecisionSetV1;
  bids: TrustedServerAuctionBidV1[];
}

/* Projection and render-source contracts live in core/contracts/auction_projection.ts. */

/** Parse the coordinated-cutover `/auction` wire without activating it in production yet. */
export function parseTrustedServerAuctionResponseV1(
  value: unknown
): TrustedServerAuctionResponseV1 | undefined {
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
    if (!seat || !isAuctionProviderIdV1(seat.seat)) return undefined;
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
        !validBoundedString(bid.impid, 256) ||
        !isAuctionCandidateIdV1(trusted.candidate_id) ||
        trusted.slot_id !== bid.impid ||
        typeof bid.price !== 'number' ||
        !Number.isFinite(bid.price) ||
        bid.price < 0 ||
        typeof bid.w !== 'number' ||
        typeof bid.h !== 'number'
      ) {
        return undefined;
      }
      const renderSource = parseRenderSource(trusted.render_source);
      if (!renderSource || renderSource.width !== bid.w || renderSource.height !== bid.h) {
        return undefined;
      }
      if (
        (renderSource.type === 'pbs_cache' && bid.id !== renderSource.cacheId) ||
        (renderSource.type !== 'pbs_cache' &&
          (!isRendererReservationIdV1(bid.id) || !validDimension(bid.w) || !validDimension(bid.h)))
      ) {
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
      const base = {
        candidateId: trusted.candidate_id,
        impid: bid.impid,
        provider: seat.seat,
        price: bid.price,
        width: bid.w,
        height: bid.h,
      };
      if (renderSource.type === 'pbs_cache') {
        bids.push({ ...base, renderSource });
      } else {
        bids.push({
          ...base,
          rendererReservationId: bid.id as string,
          renderSource,
          ...(renderSource.type === 'adm' ? { adm: renderSource.adm } : {}),
        });
      }
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
      if (
        candidates.has(bid.candidateId) ||
        ('rendererReservationId' in bid && reservations.has(bid.rendererReservationId))
      ) {
        return true;
      }
      candidates.add(bid.candidateId);
      if ('rendererReservationId' in bid) reservations.add(bid.rendererReservationId);
      const winner = winners.find((entry) => entry.candidateId === bid.candidateId);
      return !winner || winner.slot !== bid.impid;
    }) ||
    winners.some((winner) => !bids.some((bid) => bid.candidateId === winner.candidateId))
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

  const canonicalBids: BrowserAuctionProjectionV1['bids'] = [];
  for (let index = 0; index < orderedBids.length; index += 1) {
    const bid = orderedBids[index];
    if (!bid) return undefined;
    const base = {
      candidateId: bid.candidateId,
      slot: bid.impid,
      provider: bid.provider,
      cpm: bid.price,
      currency: 'USD' as const,
      targeting: {},
    };
    if (!('rendererReservationId' in bid)) {
      canonicalBids.push({
        ...base,
        upstreamBidId: bid.renderSource.cacheId,
        renderSource: bid.renderSource,
      });
    } else {
      canonicalBids.push({
        ...base,
        upstreamBidId:
          bid.renderSource.type === 'aps' ? bid.renderSource.bidId : bid.rendererReservationId,
        rendererReservationId: bid.rendererReservationId,
        renderSource: bid.renderSource,
      });
    }
  }
  const canonicalProjection: BrowserAuctionProjectionV1 = {
    version: 1,
    auction,
    // Direct `/auction` units are programmatic DOM placements, not GAM slots.
    slots: [],
    bids: canonicalBids,
  };
  if (jsonUtf8ByteLength(canonicalProjection) > MAX_BROWSER_AUCTION_PROJECTION_BYTES) {
    return undefined;
  }

  return { auction, bids: orderedBids };
}

// ---------------------------------------------------------------------------
// AdRequest building
// ---------------------------------------------------------------------------

/**
 * Build an {@link AdRequest} from an array of ad-unit-like objects.
 *
 * Accepts direct-auction programmatic units and Prebid-style `BidRequest`
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
