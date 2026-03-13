// Shared auction module: builds AdRequest payloads, sends them to /auction,
// and parses OpenRTB seatbid responses.  Used by both the core requestAds flow
// and the Prebid.js trustedServer adapter.

import { log } from './log';

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

/** The payload POSTed to the /auction orchestrator. */
export interface AdRequest {
  adUnits: AdRequestUnit[];
  config?: Record<string, unknown>;
}

/** A parsed bid from an OpenRTB seatbid response. */
export interface AuctionBid {
  /** Matches the `impid` in the response — corresponds to adUnit `code`. */
  impid: string;
  /** Creative HTML (already rewritten with proxy URLs by the server). */
  adm: string;
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
export function buildAdRequest(units: any[]): AdRequest {
  const unitMap = new Map<string, AdRequestUnit>();

  for (const u of units) {
    const code: string = u.adUnitCode ?? u.code ?? '';
    if (!unitMap.has(code)) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const mediaTypes: any = {};
      if (u.mediaTypes?.banner) {
        mediaTypes.banner = {
          sizes: u.mediaTypes.banner.sizes ?? u.sizes ?? [],
        };
      }
      unitMap.set(code, { code, mediaTypes, bids: [] });
    }

    // If the source object carries a `bidder` field (Prebid BidRequest style),
    // add it as a bid entry.  Otherwise copy the existing `bids` array.
    if (u.bidder) {
      unitMap.get(code)!.bids.push({
        bidder: u.bidder,
        params: u.params ?? {},
      });
    } else if (Array.isArray(u.bids)) {
      for (const b of u.bids) {
        unitMap.get(code)!.bids.push({
          bidder: b.bidder ?? '',
          params: b.params ?? {},
        });
      }
    }
  }

  return { adUnits: [...unitMap.values()] };
}

// ---------------------------------------------------------------------------
// OpenRTB response parsing
// ---------------------------------------------------------------------------

/**
 * Parse an OpenRTB-style response body into a flat array of {@link AuctionBid}.
 *
 * Expected shape:
 * ```json
 * { "seatbid": [{ "seat": "bidder", "bid": [{ "impid", "price", "adm", ... }] }] }
 * ```
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function parseAuctionResponse(body: any): AuctionBid[] {
  const bids: AuctionBid[] = [];
  const seatbids = body?.seatbid;
  if (!Array.isArray(seatbids)) return bids;

  for (const sb of seatbids) {
    const seat: string = sb.seat ?? 'unknown';
    const sbBids = sb.bid;
    if (!Array.isArray(sbBids)) continue;

    for (const b of sbBids) {
      bids.push({
        impid: b.impid ?? '',
        adm: b.adm ?? '',
        price: b.price ?? 0,
        width: b.w ?? 300,
        height: b.h ?? 250,
        seat,
        creativeId: b.crid ?? `${seat}-${b.impid ?? ''}`,
        adomain: Array.isArray(b.adomain) ? b.adomain : [],
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
    const res = await fetch(endpoint, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      credentials: 'same-origin',
      body: JSON.stringify(request),
      keepalive: true,
    });

    const ct = res.headers.get('content-type') || '';
    if (res.ok && ct.includes('application/json')) {
      const data: unknown = await res.json();
      const bids = parseAuctionResponse(data);
      log.info('auction: received bids', { count: bids.length });
      return bids;
    }

    log.warn('auction: unexpected response', { ok: res.ok, status: res.status, ct });
    return [];
  } catch (err) {
    log.warn('auction: request failed', err);
    return [];
  }
}
