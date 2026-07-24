// Prebid.js bundle with a custom "trustedServer" bid adapter that routes all
// bid requests through the Trusted Server /auction orchestrator endpoint.
//
// Instead of using prebidServerBidAdapter (which sends OpenRTB directly to PBS),
// we register a client-side adapter that:
//   1. Converts Prebid bid requests → AdRequest format via core/auction
//   2. POSTs to /auction (the Trusted Server orchestrator)
//   3. Parses the OpenRTB seatbid response via core/auction
//   4. Maps parsed AuctionBids into Prebid bid response objects
//
// The shim on requestBids injects "trustedServer" into every ad unit so all
// bids flow through the orchestrator.

import pbjs from 'prebid.js';
import adapterManager from 'prebid.js/src/adapterManager.js';
import { markBidAsRendered, markWinner } from 'prebid.js/src/adRendering.js';
import 'prebid.js/modules/consentManagementTcf.js';
import 'prebid.js/modules/consentManagementGpp.js';
import 'prebid.js/modules/consentManagementUsp.js';
import 'prebid.js/modules/userId.js';

// Client-side bid adapters — self-register with prebid.js on import.
// The external bundle generator aliases these placeholder modules to temporary
// modules built from its --adapters and --user-id-modules options. When a bidder
// is listed in `client_side_bidders` in trusted-server.toml, the requestBids
// shim leaves its bids untouched and the corresponding adapter handles them
// natively in the browser.
import './_adapters.generated';

import { log } from '../../core/log';
import { buildAdRequest, parseAuctionResponse, parseAuctionTraceSummary } from '../../core/auction';
import { registerApsPrebidRenderer } from '../aps/render';
import type { AuctionBid, AuctionEid } from '../../core/auction';
import type { AdTraceEventKind, AuctionSlot, TrustedServerBidTrace } from '../../core/types';

import { INCLUDED_PREBID_USER_ID_MODULES } from './_user_ids.generated';
import { PREBID_USER_ID_MODULE_REGISTRY } from './user_id_modules';

const ADAPTER_CODE = 'trustedServer';
const APS_BIDDER_CODE = 'aps';
const APS_RENDERER_FIELD = 'trustedServerRenderer';
const APS_BID_RESPONSE_LISTENER_SENTINEL = '__tsApsBidResponseListenerInstalled';
// OpenRTB permits vendor-specific agent types; PAIR uses 571187.
// Keep this range aligned with the signed 32-bit Rust/OpenRTB representation.
const MAX_OPENRTB_ATYPE = 2_147_483_647;
const BIDDER_PARAMS_KEY = 'bidderParams';
const ZONE_KEY = 'zone';
const TS_REFRESH_TARGETING_KEYS = [
  'ts_initial',
  'hb_pb',
  'hb_bidder',
  'hb_adid',
  'hb_cache_host',
  'hb_cache_path',
  'ts_trace',
] as const;
const PUBLISHER_DELIVERY_CONTEXT_TIMEOUT_MS = 1000;

/** Configuration options for the Prebid integration. */
export interface PrebidNpmConfig {
  /** Auction endpoint path. Defaults to '/auction'. */
  endpoint?: string;
  /** Server-side bid timeout in milliseconds. Defaults to 1000. */
  timeout?: number;
  /** Enable Prebid.js debug logging. Defaults to false. */
  debug?: boolean;
}

/**
 * Shape of the server-injected config at `window.__tsjs_prebid`.
 * Set by the Rust IntegrationHeadInjector from trusted-server.toml values.
 */
interface InjectedPrebidConfig {
  accountId?: string;
  timeout?: number;
  debug?: boolean;
  bidders?: string[];
  /** Bidders that run client-side via native Prebid.js adapters. */
  clientSideBidders?: string[];
  /** GAM ad-unit-path suffixes excluded from refresh auctions. */
  excludedGamAdUnitPathSuffixes?: string[];
}

interface PrebidUserIdDiagnostics {
  includedModules: string[];
  configuredUserIdNames: string[];
  missingConfiguredUserIdNames: string[];
}

/** Read server-injected config from window.__tsjs_prebid, if present. */
export function getInjectedConfig(): InjectedPrebidConfig | undefined {
  if (typeof window !== 'undefined') {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return (window as any).__tsjs_prebid as InjectedPrebidConfig | undefined;
  }
  return undefined;
}

/** Collect all unique bidder codes from the provided ad units. */
export function collectBidders(adUnits: Array<{ bids?: Array<{ bidder?: string }> }>): string[] {
  const bidders = new Set<string>();
  for (const unit of adUnits) {
    if (unit.bids) {
      for (const bid of unit.bids) {
        if (bid.bidder) {
          bidders.add(bid.bidder);
        }
      }
    }
  }
  return [...bidders];
}

function configuredUserIdNamesFromConfig(config: unknown): string[] {
  const userIds = Array.isArray(config)
    ? config
    : config && typeof config === 'object'
      ? ((
          config as {
            userSync?: { userIds?: Array<{ name?: unknown }> };
            userIds?: Array<{ name?: unknown }>;
          }
        ).userSync?.userIds ?? (config as { userIds?: Array<{ name?: unknown }> }).userIds)
      : undefined;

  if (!Array.isArray(userIds)) {
    return [];
  }

  return [
    ...new Set(
      userIds
        .map((entry) => entry?.name)
        .filter((name): name is string => typeof name === 'string' && name.length > 0)
    ),
  ].sort();
}

function readConfiguredUserIdNames(): string[] {
  const getConfig = (pbjs as unknown as { getConfig?: (key?: string) => unknown }).getConfig;
  if (typeof getConfig !== 'function') {
    return [];
  }

  return configuredUserIdNamesFromConfig(getConfig('userSync.userIds')).concat(
    configuredUserIdNamesFromConfig(getConfig())
  );
}

function recordUserIdModuleDiagnostics(): PrebidUserIdDiagnostics {
  const configuredUserIdNames = [...new Set(readConfiguredUserIdNames())].sort();
  const coveredConfigNames = new Set(
    PREBID_USER_ID_MODULE_REGISTRY.filter((entry) =>
      INCLUDED_PREBID_USER_ID_MODULES.includes(entry.moduleName)
    ).flatMap((entry) => entry.configNames)
  );
  const missingConfiguredUserIdNames = configuredUserIdNames.filter(
    (name) => !coveredConfigNames.has(name)
  );

  const diagnostics: PrebidUserIdDiagnostics = {
    includedModules: [...INCLUDED_PREBID_USER_ID_MODULES],
    configuredUserIdNames,
    missingConfiguredUserIdNames,
  };

  const previouslyMissingConfiguredUserIdNames = new Set<string>();
  if (typeof window !== 'undefined') {
    const tsjsWindow = window as typeof window & {
      __tsjs_prebid_diagnostics?: { userIdModules?: PrebidUserIdDiagnostics };
    };
    for (const name of tsjsWindow.__tsjs_prebid_diagnostics?.userIdModules
      ?.missingConfiguredUserIdNames ?? []) {
      previouslyMissingConfiguredUserIdNames.add(name);
    }
    tsjsWindow.__tsjs_prebid_diagnostics = {
      ...(tsjsWindow.__tsjs_prebid_diagnostics ?? {}),
      userIdModules: diagnostics,
    };
  }

  for (const name of missingConfiguredUserIdNames) {
    if (!previouslyMissingConfiguredUserIdNames.has(name)) {
      log.warn(
        `[tsjs-prebid] configured User ID module "${name}" is not included in the external bundle`
      );
    }
  }

  return diagnostics;
}

// ---------------------------------------------------------------------------
// trustedServer bid adapter helpers
// ---------------------------------------------------------------------------

/** Resolved endpoint — set by installPrebidNpm, read by the adapter. */
let auctionEndpoint = '/auction';

// Prebid normalizes each bid into its own internal object during `addBidResponse`,
// which can drop unknown top-level fields — so the custom `trustedServerRenderer`
// descriptor set in `interpretResponse` may be gone by the time the `bidResponse`
// listener runs (observed in production: the field is absent as early as `bidAccepted`).
// To make registration independent of custom-field survival, the descriptor is also
// stashed here keyed by `requestId` (a first-class field Prebid preserves) at
// `interpretResponse` time, and the `bidResponse` listener falls back to it. Bounded.
const MAX_PENDING_APS_RENDERERS = 256;
const pendingApsRenderersByRequestId = new Map<string, unknown>();

function stashPendingApsRenderer(requestId: unknown, renderer: unknown): void {
  if (typeof requestId !== 'string' || requestId.length === 0) return;
  if (
    !pendingApsRenderersByRequestId.has(requestId) &&
    pendingApsRenderersByRequestId.size >= MAX_PENDING_APS_RENDERERS
  ) {
    const oldest = pendingApsRenderersByRequestId.keys().next().value;
    if (oldest !== undefined) pendingApsRenderersByRequestId.delete(oldest);
  }
  pendingApsRenderersByRequestId.set(requestId, renderer);
}

function takePendingApsRenderer(requestId: unknown): unknown {
  if (typeof requestId !== 'string') return undefined;
  const renderer = pendingApsRenderersByRequestId.get(requestId);
  if (renderer !== undefined) pendingApsRenderersByRequestId.delete(requestId);
  return renderer;
}

/**
 * Convert parsed {@link AuctionBid}s into Prebid bid response objects,
 * linking each bid back to the original BidRequest via `requestId`.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function auctionBidsToPrebidBids(auctionBids: AuctionBid[], bidRequests: any[]): any[] {
  // Build a lookup from impid (adUnitCode) → original bidRequest
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const requestsByCode = new Map<string, any>();
  for (const br of bidRequests) {
    const code = br.adUnitCode ?? br.code ?? '';
    if (!requestsByCode.has(code)) {
      requestsByCode.set(code, br);
    }
  }

  return auctionBids.map((bid) => {
    const origReq = requestsByCode.get(bid.impid);
    const requestId = origReq?.bidId ?? bid.impid;
    // Stash by requestId so registration survives Prebid stripping the custom field.
    if (bid.renderer) stashPendingApsRenderer(requestId, bid.renderer);
    return {
      requestId,
      cpm: bid.price,
      width: bid.width,
      height: bid.height,
      ad: bid.renderer ? '' : bid.adm,
      ...(bid.renderer ? { [APS_RENDERER_FIELD]: bid.renderer } : {}),
      ttl: 300,
      creativeId: bid.creativeId,
      netRevenue: true,
      currency: 'USD',
      bidderCode: bid.seat,
      meta: {
        advertiserDomains: bid.adomain,
      },
      ...(bid.trace
        ? {
            adserverTargeting: { ts_trace: bid.trace.bidTraceId },
            tsTrace: bid.trace,
          }
        : {}),
    };
  });
}

// ---------------------------------------------------------------------------
// Installation / shim
// ---------------------------------------------------------------------------

type PbjsConfig = Parameters<typeof pbjs.setConfig>[0];

type TrustedServerBid = { bidder?: string; params?: Record<string, unknown> };
type BannerSize = [number, number];
type TrustedServerBanner = { sizes: BannerSize[]; name?: string };
type TrustedServerAdUnit = {
  code?: string;
  mediaTypes?: { banner?: TrustedServerBanner };
  bids?: TrustedServerBid[];
};
type ClientSideBidSnapshot = { bidder: string; params: Record<string, unknown> };
type PublisherAdUnitSnapshot = {
  bidderParams: Record<string, Record<string, unknown>>;
  clientSideBids: ClientSideBidSnapshot[];
  zone?: string;
};
type PublisherDeliveryContext = {
  remainingCodes: Set<string>;
  retainForTargetedRefresh: boolean;
  cleanupTimer?: ReturnType<typeof setTimeout>;
};
type SetTargetingForGptAsync = (...args: unknown[]) => unknown;

let publisherAdUnitSnapshots = new Map<string, PublisherAdUnitSnapshot>();
let syntheticRefreshAdUnits = new WeakSet<TrustedServerAdUnit>();
const activePublisherDeliveryContexts: PublisherDeliveryContext[] = [];
type TrustedServerBidRequest = {
  adUnitCode?: string;
  code?: string;
  bidId?: string;
  auctionId?: string;
};
type TrustedServerRequest = {
  method: 'POST';
  url: string;
  data: string;
  options: { contentType: 'application/json' };
  bidRequests: TrustedServerBidRequest[];
  tsjsBidRequests: TrustedServerBidRequest[];
};

type PrebidUserIdEid = {
  source?: unknown;
  uids?: Array<{ id?: unknown; atype?: unknown; ext?: unknown }>;
};

type RefreshGptSlot = {
  getSlotElementId?: () => string;
  getAdUnitPath?: () => string;
  getTargeting?: (key: string) => string[];
  clearTargeting?: (key?: string) => RefreshGptSlot;
  getSizes?: () => unknown[];
};

const DEFAULT_REFRESH_SIZES: BannerSize[] = [
  [728, 90],
  [300, 250],
];

function sanitizeAuctionUid(uid: {
  id?: unknown;
  atype?: unknown;
  ext?: unknown;
}): AuctionEid['uids'][number] | undefined {
  if (typeof uid?.id !== 'string' || uid.id.length === 0) {
    return undefined;
  }

  const sanitizedUid: AuctionEid['uids'][number] = { id: uid.id };

  if (
    typeof uid.atype === 'number' &&
    Number.isInteger(uid.atype) &&
    uid.atype >= 0 &&
    uid.atype <= MAX_OPENRTB_ATYPE
  ) {
    sanitizedUid.atype = uid.atype;
  }

  if (uid.ext && typeof uid.ext === 'object' && !Array.isArray(uid.ext)) {
    sanitizedUid.ext = uid.ext as Record<string, unknown>;
  }

  return sanitizedUid;
}

function isDefined<T>(value: T | undefined): value is T {
  return value !== undefined;
}

function isPositiveFiniteNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value > 0;
}

function parseBannerSize(size: unknown): BannerSize | undefined {
  if (Array.isArray(size) && isPositiveFiniteNumber(size[0]) && isPositiveFiniteNumber(size[1])) {
    return [size[0], size[1]];
  }

  const gptSize = size as { getWidth?: () => unknown; getHeight?: () => unknown };
  const width = gptSize?.getWidth?.();
  const height = gptSize?.getHeight?.();
  if (isPositiveFiniteNumber(width) && isPositiveFiniteNumber(height)) {
    return [width, height];
  }

  return undefined;
}

function bannerSizesFromGptSlot(slot: RefreshGptSlot): BannerSize[] | undefined {
  const sizes = slot.getSizes?.();
  if (!Array.isArray(sizes)) {
    return undefined;
  }

  const parsedSizes = sizes.map(parseBannerSize).filter(isDefined);
  return parsedSizes.length > 0 ? parsedSizes : undefined;
}

function bannerSizesFromInjectedSlot(slot: AuctionSlot | undefined): BannerSize[] | undefined {
  const parsedSizes = slot?.formats?.map(parseBannerSize).filter(isDefined) ?? [];
  return parsedSizes.length > 0 ? parsedSizes : undefined;
}

function refreshSlotElementId(slot: RefreshGptSlot): string | undefined {
  const elementId = slot.getSlotElementId?.();
  return elementId && elementId.length > 0 ? elementId : undefined;
}

function findInjectedSlotForRefresh(slot: RefreshGptSlot): AuctionSlot | undefined {
  const elementId = refreshSlotElementId(slot);
  if (!elementId) {
    return undefined;
  }

  const slots = window.tsjs?.adSlots;
  if (!slots) {
    return undefined;
  }

  // Prefer an exact (or container) match across all slots before the prefix
  // fallback, so prefix-overlapping div_ids (e.g. "ad" and "ad-header") resolve
  // to the correct slot instead of the first slot whose div_id is a prefix.
  return (
    slots.find(
      (adSlot) => elementId === adSlot.div_id || elementId === `${adSlot.div_id}-container`
    ) ?? slots.find((adSlot) => adSlot.div_id.length > 0 && elementId.startsWith(adSlot.div_id))
  );
}

function firstTargetingValue(values: string[] | undefined): string | undefined {
  return values?.find((value) => value.length > 0);
}

/**
 * Find the publisher's original `pbjs.adUnits` entry for a refreshing slot.
 *
 * A TS-owned GPT slot may be defined on `${div_id}-container`, so the GPT
 * element id used as the synthetic refresh ad unit code can differ from the
 * inner `div_id` the publisher keyed their Prebid ad unit by. Try each candidate
 * code in order and return the first matching ad unit, so container-backed slots
 * still recover the publisher's configured params and bidders.
 */
function findRefreshSnapshot(
  candidateCodes: Array<string | undefined>
): PublisherAdUnitSnapshot | undefined {
  for (const code of candidateCodes) {
    if (!code) continue;
    const snapshot = publisherAdUnitSnapshots.get(code);
    if (snapshot) return snapshot;
  }
  return undefined;
}

function findRefreshAdUnit(
  candidateCodes: Array<string | undefined>
): TrustedServerAdUnit | undefined {
  const adUnits = (pbjs.adUnits ?? []) as TrustedServerAdUnit[];
  for (const code of candidateCodes) {
    if (!code) continue;
    const match = adUnits.find((unit) => unit.code === code);
    if (match) return match;
  }
  return undefined;
}

function copyParamValue(value: unknown, seen = new WeakMap<object, unknown>()): unknown {
  if (Array.isArray(value)) {
    const existing = seen.get(value);
    if (existing) return existing;
    const copy: unknown[] = [];
    seen.set(value, copy);
    value.forEach((entry) => copy.push(copyParamValue(entry, seen)));
    return copy;
  }

  if (value && typeof value === 'object') {
    const prototype = Object.getPrototypeOf(value);
    if (prototype !== Object.prototype && prototype !== null) return value;

    const existing = seen.get(value);
    if (existing) return existing;
    const copy = Object.create(prototype) as Record<string, unknown>;
    seen.set(value, copy);
    for (const [key, entry] of Object.entries(value)) {
      Object.defineProperty(copy, key, {
        value: copyParamValue(entry, seen),
        enumerable: true,
        configurable: true,
        writable: true,
      });
    }
    return copy;
  }

  return value;
}

function copyParams(params: Record<string, unknown> | undefined): Record<string, unknown> {
  return copyParamValue(params ?? {}) as Record<string, unknown>;
}

function foldedBidderParams(
  bid: TrustedServerBid | undefined
): Record<string, Record<string, unknown>> {
  const folded = (bid?.params?.[BIDDER_PARAMS_KEY] ?? {}) as Record<
    string,
    Record<string, unknown>
  >;
  return Object.fromEntries(
    Object.entries(folded).map(([bidder, params]) => [bidder, copyParams(params)])
  );
}

function capturePublisherAdUnitSnapshot(
  unit: TrustedServerAdUnit,
  clientSideBidders: Set<string>
): PublisherAdUnitSnapshot | undefined {
  if (typeof unit.code !== 'string' || unit.code.length === 0) return undefined;

  const rawBidderParams: Record<string, Record<string, unknown>> = {};
  const clientSideBids: ClientSideBidSnapshot[] = [];
  let existingTsBid: TrustedServerBid | undefined;

  const bids = Array.isArray(unit.bids) ? unit.bids : [];
  for (const bid of bids) {
    if (!bid?.bidder) continue;
    if (bid.bidder === ADAPTER_CODE) {
      existingTsBid ??= bid;
      continue;
    }
    if (clientSideBidders.has(bid.bidder)) {
      clientSideBids.push({ bidder: bid.bidder, params: copyParams(bid.params) });
      continue;
    }
    rawBidderParams[bid.bidder] = copyParams(bid.params);
  }

  const bidderParams =
    Object.keys(rawBidderParams).length > 0 ? rawBidderParams : foldedBidderParams(existingTsBid);
  const zone = unit.mediaTypes?.banner?.name;

  return {
    bidderParams,
    clientSideBids,
    ...(zone ? { zone } : {}),
  };
}

/**
 * Collect the configured client-side bidder entries for a refreshing slot.
 *
 * Synthetic refresh ad units carry only the `trustedServer` bid. The
 * `requestBids` shim preserves a client-side bidder only when its bid entry is
 * already present on the ad unit, so without re-attaching them here publishers
 * that split demand between server-side and native Prebid adapters would lose
 * all client-side demand on refresh/scroll impressions. Bids are sourced from
 * the matching `pbjs.adUnits` entry (by candidate ad unit code) so the
 * publisher's configured params are preserved.
 */
function clientSideBidsForRefresh(
  candidateCodes: Array<string | undefined>
): Array<{ bidder: string; params: Record<string, unknown> }> {
  const snapshot = findRefreshSnapshot(candidateCodes);
  if (snapshot) {
    return snapshot.clientSideBids.map((bid) => ({
      bidder: bid.bidder,
      params: copyParams(bid.params),
    }));
  }

  const clientSideBidders = new Set(getInjectedConfig()?.clientSideBidders ?? []);
  if (clientSideBidders.size === 0) return [];

  const match = findRefreshAdUnit(candidateCodes);
  if (!match?.bids) return [];

  const bids: Array<{ bidder: string; params: Record<string, unknown> }> = [];
  for (const bid of match.bids) {
    if (bid?.bidder && clientSideBidders.has(bid.bidder)) {
      bids.push({ bidder: bid.bidder, params: copyParams(bid.params) });
    }
  }
  return bids;
}

/**
 * Recover the publisher's inline server-side (PBS) bidder params for a slot.
 *
 * The synthetic refresh ad unit carries only the `trustedServer` bid, so the
 * `requestBids` shim has no original server-side bidder entries to collect into
 * `bidderParams` — without this, refresh/scroll `/auction` requests send `{}`
 * and lose demand the publisher configured only on the initial ad unit. Source
 * the params from the matching `pbjs.adUnits` entry by candidate code, covering
 * both states the initial auction can leave that entry in:
 *   - raw server-side bidder entries (`{ bidder, params }`) not yet folded, and
 *   - params already folded into that unit's `trustedServer` bid `bidderParams`
 *     by a prior `requestBids` call.
 */
function serverSideBidderParamsForRefresh(
  candidateCodes: Array<string | undefined>
): Record<string, Record<string, unknown>> {
  const snapshot = findRefreshSnapshot(candidateCodes);
  if (snapshot) {
    return Object.fromEntries(
      Object.entries(snapshot.bidderParams).map(([bidder, params]) => [bidder, copyParams(params)])
    );
  }

  const match = findRefreshAdUnit(candidateCodes);
  if (!match?.bids) return {};

  const clientSideBidders = new Set(getInjectedConfig()?.clientSideBidders ?? []);
  const params: Record<string, Record<string, unknown>> = {};

  for (const bid of match.bids) {
    if (!bid?.bidder) continue;
    if (bid.bidder === ADAPTER_CODE) {
      // Params captured and folded onto the trustedServer bid by an earlier
      // requestBids call.
      const folded = (bid.params?.[BIDDER_PARAMS_KEY] ?? {}) as Record<
        string,
        Record<string, unknown>
      >;
      for (const [bidder, bidderParams] of Object.entries(folded)) {
        params[bidder] = bidderParams;
      }
      continue;
    }
    if (clientSideBidders.has(bid.bidder)) continue;
    // Raw server-side bidder entry not yet folded by the shim.
    params[bid.bidder] = bid.params ?? {};
  }

  return params;
}

function isExcludedFromRefreshAuction(
  slot: RefreshGptSlot,
  excludedGamAdUnitPathSuffixes: Set<string>
): boolean {
  if (excludedGamAdUnitPathSuffixes.size === 0) return false;

  try {
    const adUnitPath = slot.getAdUnitPath?.();
    return (
      typeof adUnitPath === 'string' &&
      [...excludedGamAdUnitPathSuffixes].some((suffix) => adUnitPath.endsWith(suffix))
    );
  } catch {
    // GPT path metadata is optional for this optimization. If it is unavailable,
    // preserve normal refresh-auction behavior rather than suppressing demand.
    return false;
  }
}

function installAdTracePrebidObservers(): void {
  const ts = window.tsjs;
  if (!ts?.recordAdTrace) return;
  const instrumented = pbjs as unknown as {
    __tsAdTraceObserved?: boolean;
    onEvent?: (event: string, handler: (data: Record<string, unknown>) => void) => void;
    setTargetingForGPTAsync?: (codes?: string[]) => unknown;
  };
  if (instrumented.__tsAdTraceObserved) return;
  instrumented.__tsAdTraceObserved = true;
  const auctionStartedAt = new Map<string, number>();

  const record =
    (kind: AdTraceEventKind) =>
    (data: Record<string, unknown> = {}): void => {
      const nestedBid =
        data.bid && typeof data.bid === 'object'
          ? (data.bid as Record<string, unknown>)
          : undefined;
      const evidence = nestedBid ?? data;
      const slotId =
        typeof evidence.adUnitCode === 'string'
          ? evidence.adUnitCode
          : typeof evidence.code === 'string'
            ? evidence.code
            : undefined;
      const bidder =
        typeof evidence.bidderCode === 'string'
          ? evidence.bidderCode
          : typeof evidence.bidder === 'string'
            ? evidence.bidder
            : undefined;
      const auctionId =
        typeof evidence.auctionId === 'string'
          ? evidence.auctionId
          : typeof data.auctionId === 'string'
            ? data.auctionId
            : '';
      const requestId =
        typeof evidence.requestId === 'string'
          ? evidence.requestId
          : typeof evidence.adId === 'string'
            ? evidence.adId
            : '';
      const adId = typeof evidence.adId === 'string' ? evidence.adId : requestId || undefined;
      const targeting = evidence.adserverTargeting as Record<string, unknown> | undefined;
      const serverTrace = evidence.tsTrace as TrustedServerBidTrace | undefined;
      const traceToken =
        typeof targeting?.ts_trace === 'string'
          ? targeting.ts_trace
          : typeof (evidence.tsTrace as { bidTraceId?: unknown } | undefined)?.bidTraceId ===
              'string'
            ? ((evidence.tsTrace as { bidTraceId: string }).bidTraceId as string)
            : undefined;
      const ledger = (ts.prebidCorrelation ??= []);
      if (kind === 'prebid_auction_init' && auctionId) {
        auctionStartedAt.set(auctionId, performance.now());
        while (auctionStartedAt.size > 64) {
          const oldest = auctionStartedAt.keys().next().value as string | undefined;
          if (!oldest) break;
          auctionStartedAt.delete(oldest);
        }
      }
      if (kind === 'prebid_bid_response' && auctionId && slotId && requestId) {
        ledger.push({
          auctionId,
          slotId,
          requestId,
          bidder,
          adId,
          traceToken,
          serverTrace,
          events: [],
        });
        if (ledger.length > 256) ledger.shift();
      } else if (kind === 'prebid_auction_end' && auctionId) {
        const startedAt = auctionStartedAt.get(auctionId);
        auctionStartedAt.delete(auctionId);
        const prebidAuctionDurationMs =
          startedAt === undefined
            ? undefined
            : Math.max(0, Math.round(performance.now() - startedAt));
        if (prebidAuctionDurationMs !== undefined) {
          for (const entry of ledger) {
            if (entry.auctionId === auctionId)
              entry.prebidAuctionDurationMs = prebidAuctionDurationMs;
          }
        }
        const adUnits = Array.isArray(data.adUnits)
          ? (data.adUnits as Array<Record<string, unknown>>)
          : [];
        const received = Array.isArray(data.bidsReceived)
          ? (data.bidsReceived as Array<Record<string, unknown>>)
          : [];
        const slotIds = new Set<string>();
        for (const unit of adUnits) {
          if (typeof unit.code === 'string') slotIds.add(unit.code);
        }
        for (const bid of received) {
          if (typeof bid.adUnitCode === 'string') slotIds.add(bid.adUnitCode);
        }
        for (const entry of ledger) {
          if (entry.auctionId === auctionId) slotIds.add(entry.slotId);
        }
        const completed = (ts.prebidCompletedAuctions ??= []);
        completed.push({ auctionId, slotIds: [...slotIds], prebidAuctionDurationMs });
        if (completed.length > 64) completed.shift();
      } else if (kind !== 'prebid_auction_init') {
        const selected = (ts.prebidSelectedParticipants ?? []).filter(
          (entry) => performance.now() - entry.selectedAt <= 30_000
        );
        ts.prebidSelectedParticipants = selected;
        const selectedMatches =
          slotId && (requestId || adId)
            ? selected.filter(
                (entry) =>
                  entry.slotId === slotId &&
                  (!auctionId || entry.auctionId === auctionId) &&
                  (!requestId ||
                    entry.requestId === requestId ||
                    (!!adId && entry.adId === adId)) &&
                  (!traceToken || entry.traceToken === traceToken)
              )
            : [];
        if (selectedMatches.length === 1) {
          const selectedEntry = selectedMatches[0];
          ts.recordAdTrace?.({
            kind,
            slotId,
            generation: selectedEntry.generation,
            bidTraceId: selectedEntry.traceToken,
            bidder: selectedEntry.bidder ?? bidder,
            prebidAuctionDurationMs: selectedEntry.prebidAuctionDurationMs,
          });
          if (kind === 'prebid_render_succeeded' || kind === 'prebid_render_failed') {
            ts.recordAdTraceCoverage?.({ category: kind, resolution: 'correlated' });
            ts.prebidSelectedParticipants = selected.filter((entry) => entry !== selectedEntry);
          }
          return;
        }

        const matches = ledger.filter(
          (entry) =>
            (!auctionId || entry.auctionId === auctionId) &&
            (!slotId || entry.slotId === slotId) &&
            (!requestId || entry.requestId === requestId || entry.adId === adId)
        );
        if (matches.length === 1) {
          const events = (matches[0].events ??= []);
          events.push(kind);
          while (events.length > 16) events.shift();
        }
        if (kind === 'prebid_render_succeeded' || kind === 'prebid_render_failed') {
          ts.recordAdTraceCoverage?.({
            category: kind,
            resolution: selectedMatches.length > 1 ? 'ambiguous' : 'unmatched',
            reason:
              selectedMatches.length > 1
                ? 'ambiguous_prebid_terminal'
                : 'unmatched_prebid_terminal',
          });
        }
      }
      // Uncorrelated Prebid callbacks remain event-count evidence only. Their
      // publisher ad-unit code is private correlation input and must not enter
      // the browser export; exact selected participants returned above already
      // use the canonical generation slot identity.
      ts.recordAdTrace?.({ kind });
    };

  instrumented.onEvent?.('auctionInit', record('prebid_auction_init'));
  instrumented.onEvent?.('bidResponse', record('prebid_bid_response'));
  instrumented.onEvent?.('bidWon', record('prebid_bid_won'));
  instrumented.onEvent?.('auctionEnd', record('prebid_auction_end'));
  instrumented.onEvent?.('adRenderSucceeded', record('prebid_render_succeeded'));
  instrumented.onEvent?.('adRenderFailed', record('prebid_render_failed'));

  // Observe the actual selection call once. The GPT request-boundary hook reads
  // the resulting slot targeting synchronously; this wrapper never caches it.
  const original = instrumented.setTargetingForGPTAsync?.bind(pbjs);
  if (!original) return;
  instrumented.setTargetingForGPTAsync = function (codes?: string[]) {
    const result = original(codes);
    ts.recordAdTrace?.({ kind: 'prebid_targeting_selected', reason: 'targeting_applied' });
    return result;
  };
}

function clearRefreshTargeting(slot: RefreshGptSlot): void {
  if (typeof slot.clearTargeting !== 'function') return;

  for (const key of TS_REFRESH_TARGETING_KEYS) {
    slot.clearTargeting(key);
  }
}

function removePublisherDeliveryContext(context: PublisherDeliveryContext): void {
  if (context.cleanupTimer !== undefined) {
    clearTimeout(context.cleanupTimer);
    context.cleanupTimer = undefined;
  }
  const index = activePublisherDeliveryContexts.lastIndexOf(context);
  if (index >= 0) activePublisherDeliveryContexts.splice(index, 1);
}

function targetingCoversPublisherDeliveryContext(
  adUnitCodes: unknown,
  context: PublisherDeliveryContext
): boolean {
  if (adUnitCodes === undefined) return context.remainingCodes.size > 0;
  const codes = typeof adUnitCodes === 'string' ? [adUnitCodes] : adUnitCodes;
  return (
    Array.isArray(codes) &&
    codes.some((code) => typeof code === 'string' && context.remainingCodes.has(code))
  );
}

function consumeBarePublisherDeliveryContext(): boolean {
  for (let index = activePublisherDeliveryContexts.length - 1; index >= 0; index -= 1) {
    const context = activePublisherDeliveryContexts[index];
    if (context.remainingCodes.size === 0) continue;
    context.remainingCodes.clear();
    removePublisherDeliveryContext(context);
    return true;
  }
  return false;
}

function consumeExplicitPublisherDeliveryContext(targetSlots: RefreshGptSlot[]): boolean {
  if (targetSlots.length === 0) return false;

  // Publishers may include GAM-only slots in the same explicit refresh that
  // delivers a completed Prebid auction. Attribute the call to delivery when
  // any slot is covered, while consuming only the covered codes so an
  // unrelated-only refresh still follows the synthetic auction path.
  const matches = new Map<PublisherDeliveryContext, Set<string>>();
  for (const slot of targetSlots) {
    const injectedSlot = findInjectedSlotForRefresh(slot);
    const candidates = [refreshSlotElementId(slot), injectedSlot?.div_id];

    for (let index = activePublisherDeliveryContexts.length - 1; index >= 0; index -= 1) {
      const context = activePublisherDeliveryContexts[index];
      const coveredCode = candidates.find(
        (code): code is string => !!code && context.remainingCodes.has(code)
      );
      if (!coveredCode) continue;

      const contextMatches = matches.get(context) ?? new Set<string>();
      contextMatches.add(coveredCode);
      matches.set(context, contextMatches);
      break;
    }
  }

  if (matches.size === 0) return false;
  for (const [context, coveredCodes] of matches) {
    coveredCodes.forEach((code) => context.remainingCodes.delete(code));
    if (context.remainingCodes.size === 0) removePublisherDeliveryContext(context);
  }
  return true;
}

function collectAuctionEids(): AuctionEid[] | undefined {
  if (typeof pbjs.getUserIdsAsEids !== 'function') {
    return undefined;
  }

  const rawEids = (pbjs.getUserIdsAsEids() ?? []) as PrebidUserIdEid[];
  const eids: AuctionEid[] = [];

  for (const eid of rawEids) {
    if (typeof eid?.source !== 'string' || eid.source.length === 0) {
      continue;
    }

    const uids = Array.isArray(eid.uids) ? eid.uids.map(sanitizeAuctionUid).filter(isDefined) : [];

    if (uids.length === 0) {
      continue;
    }

    eids.push({ source: eid.source, uids });
  }

  return eids.length > 0 ? eids : undefined;
}

/**
 * Install the Prebid integration.
 *
 * Registers the "trustedServer" bid adapter and shims `requestBids` so every
 * ad unit is also bid on by that adapter, routing through /auction.
 *
 * Config resolution (values from later sources override earlier ones):
 * 1. `window.__tsjs_prebid` — injected by the server from trusted-server.toml
 * 2. `config` argument — explicit overrides from the publisher's JS
 */
function installApsBidResponseRegistry(): void {
  const prebid = pbjs as typeof pbjs & Record<string, unknown>;
  if (prebid[APS_BID_RESPONSE_LISTENER_SENTINEL] === true) return;

  pbjs.onEvent('bidResponse', (rawBid) => {
    const bid = rawBid as unknown as Record<string, unknown>;
    if (bid['adapterCode'] !== ADAPTER_CODE || bid['bidderCode'] !== APS_BIDDER_CODE) {
      return;
    }
    // Prefer the custom field; fall back to the requestId stash when Prebid has stripped
    // it during bid normalization (the field is often gone before this listener runs).
    const renderer = bid[APS_RENDERER_FIELD] ?? takePendingApsRenderer(bid['requestId']);
    if (renderer === undefined) {
      return;
    }

    registerApsPrebidRenderer(bid['adId'], bid['adUnitCode'], renderer, bid['ttl'], {
      markWinner: () => markWinner(rawBid),
      markRendered: () => markBidAsRendered(rawBid),
    });
    // Keep the executable capability only in the bounded, one-time registry. Prebid
    // still owns the generated ad ID and ordinary GAM targeting on this bid object.
    delete bid[APS_RENDERER_FIELD];
  });
  prebid[APS_BID_RESPONSE_LISTENER_SENTINEL] = true;
}

export function installPrebidNpm(config?: Partial<PrebidNpmConfig>): typeof pbjs {
  publisherAdUnitSnapshots = new Map();
  syntheticRefreshAdUnits = new WeakSet();
  [...activePublisherDeliveryContexts].forEach(removePublisherDeliveryContext);

  const injected = getInjectedConfig();
  const merged: PrebidNpmConfig = {
    endpoint: config?.endpoint,
    timeout: config?.timeout ?? injected?.timeout,
    debug: config?.debug ?? injected?.debug,
  };

  auctionEndpoint = merged.endpoint ?? '/auction';
  installApsBidResponseRegistry();

  // Register the trustedServer adapter using pbjs.registerBidAdapter(null, code, spec)
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (pbjs as any).registerBidAdapter(undefined, ADAPTER_CODE, {
    code: ADAPTER_CODE,
    supportedMediaTypes: ['banner'],

    isBidRequestValid(): boolean {
      return true; // All requests are valid — orchestrator handles filtering
    },

    buildRequests(validBidRequests: TrustedServerBidRequest[]): TrustedServerRequest {
      log.debug('[tsjs-prebid] buildRequests', { count: validBidRequests.length });
      const requestScopedBidRequests = [...validBidRequests];
      const hasUserIdApi = typeof pbjs.getUserIdsAsEids === 'function';
      const auctionEids = collectAuctionEids();
      if (hasUserIdApi && !auctionEids) {
        clearPrebidEidsCookie();
      }
      const payload = buildAdRequest(validBidRequests, { eids: auctionEids });
      return {
        method: 'POST',
        url: auctionEndpoint,
        data: JSON.stringify(payload),
        options: { contentType: 'application/json' },
        // Keep bid requests on the request object so interpretResponse can
        // map bids without relying on shared mutable adapter state.
        bidRequests: requestScopedBidRequests,
        tsjsBidRequests: requestScopedBidRequests,
      };
    },

    interpretResponse(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      serverResponse: any,
      request?: Partial<TrustedServerRequest>
    ) {
      const body = serverResponse?.body;
      log.debug('[tsjs-prebid] interpretResponse', { hasSeatbid: !!body?.seatbid });
      const auctionBids = parseAuctionResponse(body);
      const bidRequests = request?.tsjsBidRequests ?? request?.bidRequests ?? [];
      const summary = parseAuctionTraceSummary(body);
      if (summary && window.tsjs?.recordAdTrace) {
        const summaries = (window.tsjs.prebidServerSummaries ??= []);
        for (const bidRequest of bidRequests) {
          const auctionId = bidRequest.auctionId;
          const slotId = bidRequest.adUnitCode ?? bidRequest.code;
          if (auctionId && slotId) summaries.push({ auctionId, slotId, summary });
        }
        while (summaries.length > 64) summaries.shift();
      }
      return auctionBidsToPrebidBids(auctionBids, bidRequests);
    },
  });

  const originalRequestBids = pbjs.requestBids.bind(pbjs);

  // Bidders that should run client-side via their native Prebid.js adapters.
  // Read once from the server-injected config.
  const clientSideBidders = new Set(injected?.clientSideBidders ?? []);
  if (clientSideBidders.size > 0) {
    log.info('[tsjs-prebid] client-side bidders:', [...clientSideBidders]);
  }

  // Shim requestBids to inject the trustedServer bidder into every ad unit
  // so server-side bids flow through the /auction orchestrator while
  // client-side bidders are left untouched.
  pbjs.requestBids = function (requestObj?: Parameters<typeof originalRequestBids>[0]) {
    log.debug('[tsjs-prebid] requestBids called');
    recordUserIdModuleDiagnostics();

    const opts = requestObj || {};
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const adUnits = ((opts as any).adUnits || pbjs.adUnits || []) as TrustedServerAdUnit[];
    const isSyntheticRefresh =
      adUnits.length > 0 && adUnits.every((unit) => syntheticRefreshAdUnits.has(unit));
    const publisherAdUnitCodes = new Set<string>();

    // Ensure every ad unit has a trustedServer bid entry
    for (const unit of adUnits) {
      if (!syntheticRefreshAdUnits.has(unit)) {
        const snapshot = capturePublisherAdUnitSnapshot(unit, clientSideBidders);
        if (snapshot && unit.code) {
          publisherAdUnitSnapshots.set(unit.code, snapshot);
          publisherAdUnitCodes.add(unit.code);
        }
      }

      if (!Array.isArray(unit.bids)) {
        unit.bids = [];
      }

      // Preserve per-bidder params for server-side expansion.
      // Skip client-side bidders — they remain as standalone bids and run
      // via their native Prebid.js adapters in the browser.
      const bidderParams: Record<string, Record<string, unknown>> = {};
      for (const bid of unit.bids) {
        if (!bid?.bidder || bid.bidder === ADAPTER_CODE) {
          continue;
        }
        if (clientSideBidders.has(bid.bidder)) {
          continue;
        }
        bidderParams[bid.bidder] = bid.params ?? {};
      }

      // Keep only bids that should still execute in the browser. All other
      // bidders are routed through the trustedServer adapter.
      unit.bids = unit.bids.filter(
        (bid) => bid?.bidder === ADAPTER_CODE || clientSideBidders.has(bid?.bidder ?? '')
      );

      // WORKAROUND: Read the zone from mediaTypes.banner.name. This is NOT a
      // standard Prebid.js field — publishers must add it as a custom property
      // in their ad unit config. The server uses it to apply zone-specific
      // bid-param overrides (e.g. mapping zones to s2s placement IDs).
      // TODO: Replace with a proper zone signal once available.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const zone = (unit as any).mediaTypes?.banner?.name as string | undefined;

      const existingTsBid = unit.bids.find((b) => b.bidder === ADAPTER_CODE);
      if (existingTsBid) {
        const prevParams = { ...(existingTsBid.params ?? {}) };
        delete prevParams[ZONE_KEY];

        // On a second requestBids() with the same ad unit object, the
        // server-side bidder entries were already filtered out of unit.bids
        // by the prior call, so `bidderParams` is now empty. Retain the
        // params captured on the first call instead of overwriting them with
        // `{}`, which would drop the publisher's inline PBS params on refresh.
        const prevBidderParams = (prevParams[BIDDER_PARAMS_KEY] ?? {}) as Record<
          string,
          Record<string, unknown>
        >;
        const effectiveBidderParams =
          Object.keys(bidderParams).length > 0 ? bidderParams : prevBidderParams;

        existingTsBid.params = {
          ...prevParams,
          [BIDDER_PARAMS_KEY]: effectiveBidderParams,
          ...(zone ? { [ZONE_KEY]: zone } : {}),
        };
      } else {
        unit.bids.push({
          bidder: ADAPTER_CODE,
          params: {
            [BIDDER_PARAMS_KEY]: bidderParams,
            ...(zone ? { [ZONE_KEY]: zone } : {}),
          },
        });
      }
    }

    // Ensure the trustedServer adapter is allowed to return bids under any
    // bidder code (e.g. "mocktioneer", "appnexus") from the server-side seat.
    // Re-applied on every requestBids call so that publisher code that
    // overwrites pbjs.bidderSettings doesn't drop our setting.
    pbjs.bidderSettings = {
      ...(pbjs.bidderSettings || {}),
      [ADAPTER_CODE]: {
        ...(pbjs.bidderSettings?.[ADAPTER_CODE] || {}),
        allowAlternateBidderCodes: true,
        allowedAlternateBidderCodes: ['*'],
      },
    };

    // Chain a bidsBackHandler to collect Prebid User ID Module EIDs
    // and persist them as a cookie for backend sync.
    const originalBidsBack = opts.bidsBackHandler;
    opts.bidsBackHandler = function (...args: unknown[]) {
      syncPrebidEidsCookie();
      if (typeof originalBidsBack !== 'function') return;
      if (isSyntheticRefresh || publisherAdUnitCodes.size === 0) {
        originalBidsBack.apply(this, args as Parameters<typeof originalBidsBack>);
        return;
      }

      const context: PublisherDeliveryContext = {
        remainingCodes: new Set(publisherAdUnitCodes),
        retainForTargetedRefresh: false,
      };
      const targetingPbjs = pbjs as unknown as {
        setTargetingForGPTAsync?: SetTargetingForGptAsync;
      };
      const originalSetTargeting = targetingPbjs.setTargetingForGPTAsync;
      let targetingWrapper: SetTargetingForGptAsync | undefined;
      if (typeof originalSetTargeting === 'function') {
        targetingWrapper = (...targetingArgs: unknown[]) => {
          const result = originalSetTargeting.apply(targetingPbjs, targetingArgs);
          if (targetingCoversPublisherDeliveryContext(targetingArgs[0], context)) {
            context.retainForTargetedRefresh = true;
          }
          return result;
        };
        targetingPbjs.setTargetingForGPTAsync = targetingWrapper;
      }

      activePublisherDeliveryContexts.push(context);
      try {
        originalBidsBack.apply(this, args as Parameters<typeof originalBidsBack>);
      } finally {
        if (targetingWrapper && targetingPbjs.setTargetingForGPTAsync === targetingWrapper) {
          targetingPbjs.setTargetingForGPTAsync = originalSetTargeting;
        }
        if (context.retainForTargetedRefresh && context.remainingCodes.size > 0) {
          // Some publisher wrappers set targeting in bidsBackHandler, return,
          // and schedule the matching GPT refresh shortly afterward. Retain
          // this one-shot context only after that targeting signal, with a
          // bounded expiry so a later independent refresh remains independent.
          context.cleanupTimer = setTimeout(
            () => removePublisherDeliveryContext(context),
            PUBLISHER_DELIVERY_CONTEXT_TIMEOUT_MS
          );
        } else {
          removePublisherDeliveryContext(context);
        }
      }
    };

    return originalRequestBids(opts);
  };

  // Apply initial configuration
  const pbjsConfig: PbjsConfig & { bidderTimeout?: number } = {
    debug: merged.debug ?? false,
  };
  if (typeof merged.timeout === 'number') {
    pbjsConfig.bidderTimeout = merged.timeout;
  }
  pbjs.setConfig(pbjsConfig as PbjsConfig);

  // processQueue() must be called after all modules are loaded when using
  // prebid.js via NPM.
  pbjs.processQueue();
  recordUserIdModuleDiagnostics();
  installAdTracePrebidObservers();

  // Validate that every client-side bidder has its adapter registered.
  // Adapters self-register on import, so a missing adapter means the bidder
  // was listed in client_side_bidders but not included in the generated
  // external Prebid bundle. Without the adapter the bidder is silently dropped
  // from both server-side and client-side auctions.
  for (const bidder of clientSideBidders) {
    try {
      if (!adapterManager.getBidAdapter(bidder)) {
        log.error(
          `[tsjs-prebid] client-side bidder "${bidder}" has no adapter loaded. ` +
            `Add it to build-prebid-external.mjs --adapters.`
        );
      }
    } catch {
      log.error(
        `[tsjs-prebid] client-side bidder "${bidder}" has no adapter loaded. ` +
          `Add it to build-prebid-external.mjs --adapters.`
      );
    }
  }

  log.info('[tsjs-prebid] prebid initialized with trustedServer adapter');

  return pbjs;
}

// ─── Phase B: GPT scroll/refresh auction handler ──────────────────────────

/**
 * Install the scroll/refresh auction handler.
 *
 * Wraps `googletag.pubads().refresh()` so that when the publisher's GPT
 * refresh policy fires (sticky anchor, viewability dwell, infinite scroll),
 * Prebid runs a fresh client-side auction for the refreshing slots before
 * the GAM call. TS-owned first-impression slots (`ts_initial=1`) are included
 * on later publisher refreshes, but stale TS server-side targeting is cleared
 * before fresh Prebid targeting is applied.
 *
 * Must be called after `installPrebidNpm()` and after GPT is loaded.
 * Idempotent: safe to call multiple times — wraps only once via a sentinel.
 */
export function installRefreshHandler(timeoutMs = 1500): void {
  if (typeof window === 'undefined') return;
  const g = (
    window as unknown as {
      googletag?: {
        cmd?: { push(fn: () => void): void };
        pubads?(): {
          refresh(slots?: unknown[], opts?: unknown): void;
          getTargeting?(key: string): string[];
        };
      };
    }
  ).googletag;
  if (!g?.cmd) return;

  g.cmd.push(() => {
    const pubads = g.pubads?.();
    if (!pubads || (pubads as { __tsRefreshWrapped?: boolean }).__tsRefreshWrapped) return;
    (pubads as { __tsRefreshWrapped?: boolean }).__tsRefreshWrapped = true;

    const originalRefresh = pubads.refresh.bind(pubads);
    pubads.refresh = function (slots?: unknown[], opts?: unknown) {
      // For bare refresh() calls (no slots arg), get all registered slots from GPT
      // so we can auction the same concrete slot list and avoid stale targeting.
      const targetSlots = (
        slots ??
        (pubads as { getSlots?: () => unknown[] }).getSlots?.() ??
        []
      ).filter((slot): slot is RefreshGptSlot => typeof slot === 'object' && slot !== null);

      // One-shot bypass for adInit()'s internal refresh: that refresh delivers
      // freshly applied server-side targeting to GAM and must not be turned
      // into a client-side auction (which would clear the TS targeting).
      // Publisher-initiated refreshes of the same slots are not flagged and
      // still run a fresh client-side auction below.
      if (window.tsjs?.adInitRefreshInProgress) {
        return originalRefresh(slots, opts);
      }

      const isExplicitSlotList = slots !== undefined;
      const hasOnlyValidExplicitSlots = !isExplicitSlotList || targetSlots.length === slots.length;
      const isPublisherDeliveryRefresh = isExplicitSlotList
        ? hasOnlyValidExplicitSlots && consumeExplicitPublisherDeliveryContext(targetSlots)
        : consumeBarePublisherDeliveryContext();
      if (isPublisherDeliveryRefresh) {
        return originalRefresh(slots, opts);
      }

      if (!targetSlots.length) {
        return originalRefresh(slots, opts);
      }

      targetSlots.forEach(clearRefreshTargeting);

      const excludedGamAdUnitPathSuffixes = new Set(
        getInjectedConfig()?.excludedGamAdUnitPathSuffixes ?? []
      );
      const auctionSlots = targetSlots.filter(
        (slot) => !isExcludedFromRefreshAuction(slot, excludedGamAdUnitPathSuffixes)
      );
      if (!auctionSlots.length) {
        return originalRefresh(targetSlots, opts);
      }

      const adUnits = auctionSlots.map((slot) => {
        const injectedSlot = findInjectedSlotForRefresh(slot);
        const code = refreshSlotElementId(slot) ?? 'refresh-slot';
        // A TS-owned slot may be defined on `${div_id}-container`, so the GPT
        // element id used as the synthetic refresh code can differ from the
        // inner `div_id` the publisher keyed their ad unit by. Recover from both.
        const candidateCodes = [code, injectedSlot?.div_id];
        const snapshot = findRefreshSnapshot(candidateCodes);
        const zone =
          injectedSlot?.targeting?.[ZONE_KEY] ??
          firstTargetingValue(slot.getTargeting?.(ZONE_KEY)) ??
          snapshot?.zone;
        const banner: TrustedServerBanner = {
          sizes:
            bannerSizesFromInjectedSlot(injectedSlot) ??
            bannerSizesFromGptSlot(slot) ??
            DEFAULT_REFRESH_SIZES,
          ...(zone ? { name: zone } : {}),
        };
        const tsParams: Record<string, unknown> = zone ? { [ZONE_KEY]: zone } : {};
        // Carry the publisher's inline server-side (PBS) bidder params captured
        // on the initial ad unit so refresh/scroll auctions don't drop them.
        const serverSideParams = serverSideBidderParamsForRefresh(candidateCodes);
        if (Object.keys(serverSideParams).length > 0) {
          tsParams[BIDDER_PARAMS_KEY] = serverSideParams;
        }
        return {
          code,
          mediaTypes: { banner },
          bids: [
            { bidder: ADAPTER_CODE, params: tsParams },
            ...clientSideBidsForRefresh(candidateCodes),
          ],
        };
      });

      // Scope GPT targeting to just the synthetic refresh ad units. An unscoped
      // call would set hb_* targeting on every ad unit with known bids, mutating
      // unrelated GPT slots whose targeting this wrapper only cleared for
      // `targetSlots` — leaving their next request dependent on stale state.
      const refreshAdUnitCodes = adUnits.map((unit) => unit.code);
      adUnits.forEach((unit) => syntheticRefreshAdUnits.add(unit));
      pbjs.requestBids({
        adUnits,
        bidsBackHandler: () => {
          pbjs.setTargetingForGPTAsync?.(refreshAdUnitCodes);
          targetSlots.forEach((slot) =>
            window.tsjs?.captureAdTraceRequest?.(slot, 'prebid_refresh')
          );
          originalRefresh(targetSlots, opts);
        },
        timeout: timeoutMs,
      });
    };

    log.info('[tsjs-prebid] GPT refresh handler installed');
  });
}

/**
 * Configure identity sync behavior for the generated Prebid User ID modules.
 *
 * The external bundle generator statically imports the selected modules through
 * `_user_ids.generated.ts`. This post-window-load configuration controls when
 * those modules synchronize identities; it does not select or register modules.
 */
export function installUserIdModules(): void {
  try {
    pbjs.setConfig({
      userSync: {
        syncEnabled: true,
        filterSettings: {
          all: { bidders: '*', filter: 'include' },
        },
        auctionDelay: 0,
        syncsPerBidder: 5,
        syncDelay: 3000,
      },
    });
    log.info('[tsjs-prebid] userID modules configured');
  } catch {
    // pbjs not ready — userID modules will use defaults
  }
}

// ---------------------------------------------------------------------------
// Prebid EID cookie sync
// ---------------------------------------------------------------------------

/** Maximum cookie payload size in bytes (leave room for other cookies). */
const MAX_EID_COOKIE_BYTES = 3072;

/** Cookie name for persisted Prebid EIDs. */
const EID_COOKIE_NAME = 'ts-eids';

/** Cookie max-age in seconds (1 day). */
const EID_COOKIE_MAX_AGE = 86400;

/** Clears any previously persisted Prebid EIDs cookie. */
function clearPrebidEidsCookie(): void {
  document.cookie = `${EID_COOKIE_NAME}=; Path=/; Secure; SameSite=Lax; Max-Age=0`;
}

function fitAuctionEidsToCookie(eids: AuctionEid[]): AuctionEid[] | undefined {
  let payload = eids.map((eid) => ({ source: eid.source, uids: [...eid.uids] }));

  while (payload.length > 0) {
    const encoded = btoa(JSON.stringify(payload));
    if (encoded.length <= MAX_EID_COOKIE_BYTES) {
      return payload;
    }

    const last = payload[payload.length - 1];
    if (last && last.uids.length > 1) {
      last.uids = last.uids.slice(0, last.uids.length - 1);
      continue;
    }

    payload = payload.slice(0, payload.length - 1);
  }

  return undefined;
}

/**
 * Collects EIDs from Prebid's User ID Module and writes them as a
 * base64-encoded OpenRTB-style JSON cookie (`ts-eids`) for backend ingestion
 * and auction fallback on later requests.
 */
function syncPrebidEidsCookie(): void {
  try {
    if (typeof pbjs.getUserIdsAsEids !== 'function') {
      // Without Prebid EIDs to forward, stale auction fallback IDs must not persist.
      clearPrebidEidsCookie();
      return;
    }

    const eids = collectAuctionEids();
    if (!eids) {
      clearPrebidEidsCookie();
      return;
    }

    const payload = fitAuctionEidsToCookie(eids);
    if (!payload) {
      clearPrebidEidsCookie();
      return;
    }

    const encoded = btoa(JSON.stringify(payload));
    document.cookie = `${EID_COOKIE_NAME}=${encoded}; Path=/; Secure; SameSite=Lax; Max-Age=${EID_COOKIE_MAX_AGE}`;

    log.debug(`[tsjs-prebid] synced ${payload.length} EID sources to cookie`);
  } catch (err) {
    log.warn('[tsjs-prebid] failed to sync EIDs cookie', err);
  }
}

// Self-initialize when loaded in a browser (same pattern as other integrations).
if (typeof window !== 'undefined') {
  installPrebidNpm();
  installRefreshHandler();
  // The slim-Prebid lazy loader appends this bundle from a window.load
  // handler, so `load` may already have fired by the time this code runs —
  // waiting for it again would skip user ID setup entirely on that path.
  if (document.readyState === 'complete') {
    installUserIdModules();
  } else {
    window.addEventListener(
      'load',
      () => {
        installUserIdModules();
      },
      { once: true }
    );
  }
}

export { pbjs };
export default installPrebidNpm;
