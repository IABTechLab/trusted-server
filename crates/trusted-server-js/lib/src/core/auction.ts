// Shared auction module: builds AdRequest payloads, sends them to /auction,
// and parses OpenRTB seatbid responses.  Used by both the core requestAds flow
// and the Prebid.js trustedServer adapter.

import { log } from './log';
import type {
  AuctionTraceOutcome,
  AuctionTraceSource,
  AuctionTraceSummary,
  TrustedServerBidTrace,
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
export type AuctionClientResult =
  | { kind: 'ok'; summary?: AuctionTraceSummary; bids: AuctionBid[] }
  | { kind: 'transport_error'; reason: 'network' | 'http' }
  | { kind: 'invalid_response'; reason: 'non_json' | 'invalid_shape' };

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
  /** Tester-gated trace joined to the validated root summary. */
  trace?: TrustedServerBidTrace;
}

const TRACE_UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const TRACE_LABEL_RE = /^[\w.-]{1,64}$/;
const TRACE_SOURCES = new Set<AuctionTraceSource>([
  'initial_navigation',
  'spa_navigation',
  'auction_api',
]);
const TRACE_OUTCOMES = new Set<AuctionTraceOutcome>([
  'completed',
  'no_bid',
  'skipped',
  'failed',
  'abandoned',
]);

/** Strictly parse the optional Trusted Server root extension. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function parseAuctionTraceSummary(body: any): AuctionTraceSummary | undefined {
  const trace = body?.ext?.trusted_server?.trace;
  if (
    trace?.version !== 1 ||
    !TRACE_UUID_RE.test(trace.auction_trace_id) ||
    !TRACE_SOURCES.has(trace.source) ||
    !TRACE_OUTCOMES.has(trace.outcome)
  ) {
    return undefined;
  }
  return {
    version: 1,
    auctionTraceId: trace.auction_trace_id,
    source: trace.source,
    outcome: trace.outcome,
  };
}

function parseBidTrace(
  bid: any, // eslint-disable-line @typescript-eslint/no-explicit-any
  root: AuctionTraceSummary | undefined
): TrustedServerBidTrace | undefined {
  const trace = bid?.ext?.trusted_server?.trace;
  if (
    !root ||
    root.outcome !== 'completed' ||
    trace?.version !== 1 ||
    !TRACE_UUID_RE.test(trace.bid_trace_id) ||
    typeof trace.slot_id !== 'string' ||
    trace.slot_id !== bid?.impid ||
    !TRACE_LABEL_RE.test(trace.slot_id) ||
    !TRACE_LABEL_RE.test(trace.provider) ||
    !TRACE_LABEL_RE.test(trace.bidder)
  ) {
    return undefined;
  }
  return {
    version: 1,
    auctionTraceId: root.auctionTraceId,
    bidTraceId: trace.bid_trace_id,
    source: root.source,
    slotId: trace.slot_id,
    provider: trace.provider,
    bidder: trace.bidder,
  };
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
 * Expected shape:
 * ```json
 * { "seatbid": [{ "seat": "bidder", "bid": [{ "impid", "price", "adm", ... }] }] }
 * ```
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function parseAuctionResponse(body: any): AuctionBid[] {
  const bids: AuctionBid[] = [];
  const rootTrace = parseAuctionTraceSummary(body);
  const seatbids = body?.seatbid;
  if (!Array.isArray(seatbids)) return bids;

  for (const sb of seatbids) {
    const seat: string = sb.seat ?? 'unknown';
    const sbBids = sb.bid;
    if (!Array.isArray(sbBids)) continue;

    for (const b of sbBids) {
      // Coerce missing/null adm to '' so AuctionBid.adm is always a string.
      // The empty-string case is filtered in renderCreativeInline via the
      // `if (!bid.adm)` guard. The client-side `typeof !== 'string'` check in
      // sanitizeCreativeHtml is a second line of defense for callers that bypass
      // parseAuctionResponse and pass untrusted values directly.
      const trace = parseBidTrace(b, rootTrace);
      bids.push({
        impid: b.impid ?? '',
        adm: b.adm ?? '',
        price: b.price ?? 0,
        width: b.w ?? 300,
        height: b.h ?? 250,
        seat,
        creativeId: b.crid ?? `${seat}-${b.impid ?? ''}`,
        adomain: Array.isArray(b.adomain) ? b.adomain : [],
        ...(trace ? { trace } : {}),
      });
    }
  }
  return bids;
}

function isValidAuctionResponseShape(data: Record<string, unknown>): boolean {
  const seatbid = data.seatbid;
  // Preserve the legacy valid empty response while rejecting a present but
  // malformed collection that would otherwise be misreported as no-bid.
  if (seatbid === undefined) return true;
  if (!Array.isArray(seatbid)) return false;
  return seatbid.every((seat) => {
    if (!seat || typeof seat !== 'object' || Array.isArray(seat)) return false;
    const bids = (seat as Record<string, unknown>).bid;
    return (
      bids === undefined ||
      (Array.isArray(bids) &&
        bids.every((bid) => !!bid && typeof bid === 'object' && !Array.isArray(bid)))
    );
  });
}

// ---------------------------------------------------------------------------
// Auction HTTP call
// ---------------------------------------------------------------------------

/**
 * POST an {@link AdRequest} and distinguish a valid empty auction from
 * transport or response-shape failures.
 */
export async function sendAuction(
  endpoint: string,
  request: AdRequest
): Promise<AuctionClientResult> {
  if (typeof fetch !== 'function') {
    log.warn('auction: fetch not available');
    return { kind: 'transport_error', reason: 'network' };
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
    if (!res.ok) {
      log.warn('auction: unexpected response', { ok: res.ok, status: res.status, ct });
      return { kind: 'transport_error', reason: 'http' };
    }
    if (!ct.includes('application/json')) {
      log.warn('auction: non-json response', { status: res.status, ct });
      return { kind: 'invalid_response', reason: 'non_json' };
    }

    let data: unknown;
    try {
      data = await res.json();
    } catch (err) {
      log.warn('auction: invalid json response', err);
      return { kind: 'invalid_response', reason: 'non_json' };
    }
    if (
      !data ||
      typeof data !== 'object' ||
      Array.isArray(data) ||
      !isValidAuctionResponseShape(data as Record<string, unknown>)
    ) {
      return { kind: 'invalid_response', reason: 'invalid_shape' };
    }
    const bids = parseAuctionResponse(data);
    const summary = parseAuctionTraceSummary(data);
    log.info('auction: received bids', { count: bids.length });
    return { kind: 'ok', ...(summary ? { summary } : {}), bids };
  } catch (err) {
    log.warn('auction: request failed', err);
    return { kind: 'transport_error', reason: 'network' };
  }
}
