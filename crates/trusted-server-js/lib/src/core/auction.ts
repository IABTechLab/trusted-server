// Shared auction module: builds AdRequest payloads, sends them to /auction,
// and parses OpenRTB seatbid responses. Used by both the core requestAds flow
// and the Prebid.js trustedServer adapter.

import { parseApsRendererDescriptor } from '../integrations/aps/render';

import { log } from './log';
import type { ApsRendererV1 } from './types';

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
  adm: string;
  /** Typed APS renderer descriptor, when the bid does not carry `adm`. */
  renderer?: ApsRendererV1;
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
  /** Server-side auction ID (response top-level `id` / `ext.ts.auction_id`). */
  auctionId?: string;
  /** Trace hash of the delivered adm (`ext.ts.adm_hash`, 16 hex chars of SHA-256). */
  admHash?: string;
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
      const trace = bid?.ext?.ts;
      const impid = typeof bid?.impid === 'string' ? bid.impid : '';
      const renderer = parseApsRendererDescriptor(bid?.ext?.trusted_server?.renderer);
      const width = typeof bid?.w === 'number' ? bid.w : (renderer?.width ?? 300);
      const height = typeof bid?.h === 'number' ? bid.h : (renderer?.height ?? 250);
      const creativeId =
        typeof bid?.crid === 'string' ? bid.crid : (renderer?.creativeId ?? `${seat}-${impid}`);

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
        auctionId:
          typeof trace?.auction_id === 'string' && trace.auction_id !== ''
            ? trace.auction_id
            : responseAuctionId,
        admHash:
          typeof trace?.adm_hash === 'string' && trace.adm_hash !== '' ? trace.adm_hash : undefined,
        adomain: Array.isArray(bid?.adomain)
          ? bid.adomain.filter((domain: unknown): domain is string => typeof domain === 'string')
          : [],
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
