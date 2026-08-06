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

import type _pbjsDefault from 'prebid.js';

import { log } from '../../core/log';
import { buildAdRequest, parseAuctionResponse } from '../../core/auction';
import type { AuctionBid, AuctionEid } from '../../core/auction';
import type { AuctionSlot, TsjsApi } from '../../core/types';

import { PREBID_USER_ID_MODULE_REGISTRY } from './user_id_modules';

/**
 * Prebid.js public API surface (type-only; erased at build time).
 *
 * `getUserIdsAsEids` is added by the userId module at runtime, which the base
 * package typing does not model.
 */
type PbjsGlobal = typeof _pbjsDefault & {
  getUserIdsAsEids?: () => unknown[];
};

// Prebid.js itself is NOT bundled into this module. It is served as the
// external bundle configured via `integrations.prebid.external_bundle_url`
// (required whenever the prebid integration is enabled) and owns the
// `window.pbjs` global. The Rust head injector emits a stub
// (`window.pbjs = window.pbjs || {que:[],cmd:[]}`) before any script runs and
// Prebid.js installs its API onto that same object, so capturing the reference
// at module scope is safe regardless of evaluation order.
const pbjs: PbjsGlobal = (
  typeof window !== 'undefined'
    ? // eslint-disable-next-line @typescript-eslint/no-explicit-any
      ((window as any).pbjs ??= { que: [], cmd: [] })
    : { que: [], cmd: [] }
) as PbjsGlobal;

/**
 * Manifest stamped on `window.__tsjs_prebid_bundle` by the external Prebid.js
 * bundle (see build-prebid-external.mjs): which client-side bid adapters and
 * user ID modules were compiled into it.
 */
interface ExternalPrebidBundleManifest {
  adapters?: string[];
  bidderCodes?: string[];
  userIdModules?: string[];
}

function sanitizeManifestList(value: unknown): string[] | undefined {
  if (!Array.isArray(value)) {
    return undefined;
  }
  return value.filter((entry): entry is string => typeof entry === 'string');
}

function getExternalBundleManifest(): ExternalPrebidBundleManifest | undefined {
  if (typeof window === 'undefined') {
    return undefined;
  }
  // The manifest is a plain window global any page script can overwrite, so
  // validate its shape instead of trusting the declared type: a non-array
  // field must degrade to "not stamped" diagnostics, not a TypeError.
  const raw = (window as { __tsjs_prebid_bundle?: unknown }).__tsjs_prebid_bundle;
  if (raw === null || typeof raw !== 'object') {
    return undefined;
  }
  const manifest = raw as Record<string, unknown>;
  return {
    adapters: sanitizeManifestList(manifest.adapters),
    bidderCodes: sanitizeManifestList(manifest.bidderCodes),
    userIdModules: sanitizeManifestList(manifest.userIdModules),
  };
}

/**
 * Whether the captured `window.pbjs` carries the real Prebid.js API rather
 * than the head-injected `{ que, cmd }` stub left behind when the external
 * bundle fails to load.
 */
function hasPrebidJsApi(): boolean {
  return typeof (pbjs as { registerBidAdapter?: unknown }).registerBidAdapter === 'function';
}

const ADAPTER_CODE = 'trustedServer';
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
] as const;
const MAX_PUBLISHER_AD_UNIT_SNAPSHOTS = 256;
const MAX_PENDING_PUBLISHER_BIDS = 2048;
const PENDING_PUBLISHER_DELIVERY_TTL_MS = 5000;

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

/** Warn-once flag for an unstamped User ID manifest; reset by installPrebidNpm. */
let warnedMissingUserIdManifest = false;

function recordUserIdModuleDiagnostics(): PrebidUserIdDiagnostics {
  const manifestUserIdModules = getExternalBundleManifest()?.userIdModules;
  const includedUserIdModules = manifestUserIdModules ?? [];
  const configuredUserIdNames = [...new Set(readConfiguredUserIdNames())].sort();
  const coveredConfigNames = new Set(
    PREBID_USER_ID_MODULE_REGISTRY.filter((entry) =>
      includedUserIdModules.includes(entry.moduleName)
    ).flatMap((entry) => entry.configNames)
  );
  // An older or unstamped bundle must not make every configured module look
  // absent: warn once about the missing manifest instead of once per module,
  // mirroring the client-side adapter validation in installPrebidNpm.
  const missingConfiguredUserIdNames =
    manifestUserIdModules === undefined
      ? []
      : configuredUserIdNames.filter((name) => !coveredConfigNames.has(name));
  if (
    manifestUserIdModules === undefined &&
    configuredUserIdNames.length > 0 &&
    !warnedMissingUserIdManifest
  ) {
    warnedMissingUserIdManifest = true;
    log.warn(
      '[tsjs-prebid] external Prebid bundle did not stamp a User ID module manifest; ' +
        'cannot verify configured User ID modules'
    );
  }

  const diagnostics: PrebidUserIdDiagnostics = {
    includedModules: [...includedUserIdModules],
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
    return {
      requestId: origReq?.bidId ?? bid.impid,
      cpm: bid.price,
      width: bid.width,
      height: bid.height,
      ad: bid.adm,
      ttl: 300,
      creativeId: bid.creativeId,
      netRevenue: true,
      currency: 'USD',
      bidderCode: bid.seat,
      meta: {
        advertiserDomains: bid.adomain,
      },
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
type PendingPublisherBid = {
  adUnitCode: string;
  expiresAt: number;
  registrationId: number;
};
type PendingPublisherCode = {
  expiresAt: number;
  registrationId: number;
};
type RemoveAdUnit = (adUnitCode?: string | string[]) => unknown;
type PrebidWithRemoveAdUnit = {
  removeAdUnit?: RemoveAdUnit;
  __tsRemoveAdUnitWrapped?: boolean;
};

let publisherAdUnitSnapshots = new Map<string, PublisherAdUnitSnapshot>();
let pendingPublisherBids = new Map<string, PendingPublisherBid>();
let pendingPublisherCodes = new Map<string, PendingPublisherCode>();
let pendingPublisherRegistrationId = 0;
let syntheticRefreshAdUnits = new WeakSet<TrustedServerAdUnit>();
type TrustedServerBidRequest = {
  adUnitCode?: string;
  code?: string;
  bidId?: string;
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
  getTargeting?: (key: string) => string[];
  clearTargeting?: (key?: string) => RefreshGptSlot;
  getSizes?: () => unknown[];
};

function recordPrebidRefreshForDiagnostics(slots: RefreshGptSlot[]): void {
  try {
    window.tsjs?.gptDiagnostics?.recordPrebidRefresh?.(slots);
  } catch {
    // Diagnostics must not suppress the GAM request.
  }
}

function dispatchPrebidRefresh<T>(
  refresh: (slots?: unknown[], opts?: unknown) => T,
  slots: unknown[] | undefined,
  opts: unknown
): T {
  let tsjs: TsjsApi | undefined;
  let hadOwnContext = false;
  let previousContext: boolean | undefined;
  let contextSet = false;
  try {
    tsjs = window.tsjs;
    if (tsjs) {
      hadOwnContext = Object.prototype.hasOwnProperty.call(tsjs, 'prebidRefreshDispatchInProgress');
      previousContext = tsjs.prebidRefreshDispatchInProgress;
      tsjs.prebidRefreshDispatchInProgress = true;
      contextSet = true;
    }
  } catch {
    // Diagnostics context must not affect refresh delegation.
  }
  try {
    return refresh(slots, opts);
  } finally {
    if (contextSet && tsjs) {
      try {
        if (hadOwnContext) {
          tsjs.prebidRefreshDispatchInProgress = previousContext;
        } else {
          delete tsjs.prebidRefreshDispatchInProgress;
        }
      } catch {
        // Diagnostics context restoration must not mask a refresh result or throw.
      }
    }
  }
}

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

/** Store a snapshot and evict the least-recently used entry when capacity is exceeded. */
function storePublisherAdUnitSnapshot(code: string, snapshot: PublisherAdUnitSnapshot): void {
  publisherAdUnitSnapshots.delete(code);
  publisherAdUnitSnapshots.set(code, snapshot);

  if (publisherAdUnitSnapshots.size > MAX_PUBLISHER_AD_UNIT_SNAPSHOTS) {
    const oldestCode = publisherAdUnitSnapshots.keys().next().value;
    if (oldestCode !== undefined) publisherAdUnitSnapshots.delete(oldestCode);
  }
}

/** Find and touch a request-scoped publisher snapshot by candidate code. */
function findRefreshSnapshot(
  candidateCodes: Array<string | undefined>
): PublisherAdUnitSnapshot | undefined {
  for (const code of candidateCodes) {
    if (!code) continue;
    const snapshot = publisherAdUnitSnapshots.get(code);
    if (!snapshot) continue;
    publisherAdUnitSnapshots.delete(code);
    publisherAdUnitSnapshots.set(code, snapshot);
    return snapshot;
  }
  return undefined;
}

/**
 * Find the publisher's live `pbjs.adUnits` entry for a refreshing slot.
 *
 * A TS-owned GPT slot may be defined on `${div_id}-container`, so the GPT
 * element id used as the synthetic refresh ad unit code can differ from the
 * inner `div_id` the publisher keyed their Prebid ad unit by. Try each candidate
 * code in order and return the first matching ad unit, so container-backed slots
 * still recover the publisher's configured params and bidders.
 */
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

/** Deep-copy plain publisher params while preserving cycles and non-plain values. */
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

/** Copy bidder params previously folded into a `trustedServer` bid. */
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

/** Capture immutable request-scoped bidder and zone data before the shim mutates an ad unit. */
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
 * all client-side demand on refresh/scroll impressions. A live exact
 * `pbjs.adUnits` match is authoritative; request-scoped snapshots are used only
 * when no live unit exists.
 */
function clientSideBidsForRefresh(
  candidateCodes: Array<string | undefined>
): Array<{ bidder: string; params: Record<string, unknown> }> {
  const clientSideBidders = new Set(getInjectedConfig()?.clientSideBidders ?? []);
  const match = findRefreshAdUnit(candidateCodes);
  if (match) {
    if (clientSideBidders.size === 0 || !Array.isArray(match.bids)) return [];

    const bids: Array<{ bidder: string; params: Record<string, unknown> }> = [];
    for (const bid of match.bids) {
      if (bid?.bidder && clientSideBidders.has(bid.bidder)) {
        bids.push({ bidder: bid.bidder, params: copyParams(bid.params) });
      }
    }
    return bids;
  }

  const snapshot = findRefreshSnapshot(candidateCodes);
  return (
    snapshot?.clientSideBids.map((bid) => ({
      bidder: bid.bidder,
      params: copyParams(bid.params),
    })) ?? []
  );
}

/**
 * Recover the publisher's inline server-side (PBS) bidder params for a slot.
 *
 * The synthetic refresh ad unit carries only the `trustedServer` bid, so the
 * `requestBids` shim has no original server-side bidder entries to collect into
 * `bidderParams` — without this, refresh/scroll `/auction` requests send `{}`
 * and lose demand the publisher configured only on the initial ad unit. A live
 * exact `pbjs.adUnits` match is authoritative and covers both raw bidder entries
 * and params already folded into a `trustedServer` bid. A request-scoped
 * snapshot is used only when no live unit exists.
 */
function serverSideBidderParamsForRefresh(
  candidateCodes: Array<string | undefined>
): Record<string, Record<string, unknown>> {
  const match = findRefreshAdUnit(candidateCodes);
  if (match) {
    if (!Array.isArray(match.bids)) return {};

    const clientSideBidders = new Set(getInjectedConfig()?.clientSideBidders ?? []);
    const params: Record<string, Record<string, unknown>> = {};

    for (const bid of match.bids) {
      if (!bid?.bidder) continue;
      if (bid.bidder === ADAPTER_CODE) {
        Object.assign(params, foldedBidderParams(bid));
        continue;
      }
      if (clientSideBidders.has(bid.bidder)) continue;
      params[bid.bidder] = copyParams(bid.params);
    }

    return params;
  }

  const snapshot = findRefreshSnapshot(candidateCodes);
  return snapshot
    ? Object.fromEntries(
        Object.entries(snapshot.bidderParams).map(([bidder, params]) => [
          bidder,
          copyParams(params),
        ])
      )
    : {};
}

/** Return a live publisher zone, falling back to a request-scoped snapshot. */
function publisherZoneForRefresh(candidateCodes: Array<string | undefined>): string | undefined {
  const match = findRefreshAdUnit(candidateCodes);
  return match ? match.mediaTypes?.banner?.name : findRefreshSnapshot(candidateCodes)?.zone;
}

function clearRefreshTargeting(slot: RefreshGptSlot): void {
  if (typeof slot.clearTargeting !== 'function') return;

  for (const key of TS_REFRESH_TARGETING_KEYS) {
    slot.clearTargeting(key);
  }
}

/** Remove pending delivery state for an ad unit, optionally from one registration only. */
function removePendingPublisherBidsForCode(adUnitCode: string, registrationId?: number): void {
  const pendingCode = pendingPublisherCodes.get(adUnitCode);
  if (registrationId !== undefined && pendingCode?.registrationId !== registrationId) return;

  pendingPublisherCodes.delete(adUnitCode);
  for (const [adId, pendingBid] of pendingPublisherBids) {
    if (
      pendingBid.adUnitCode === adUnitCode &&
      (registrationId === undefined || pendingBid.registrationId === registrationId)
    ) {
      pendingPublisherBids.delete(adId);
    }
  }
}

/** Discard delivery state that outlived the publisher auction which created it. */
function prunePendingPublisherBids(now = Date.now()): void {
  for (const [adUnitCode, pendingCode] of pendingPublisherCodes) {
    if (pendingCode.expiresAt <= now) pendingPublisherCodes.delete(adUnitCode);
  }

  for (const [adId, pendingBid] of pendingPublisherBids) {
    if (pendingBid.expiresAt <= now) pendingPublisherBids.delete(adId);
  }
}

/** Store a short-lived pending publisher ad-unit code for delivery correlation. */
function storePendingPublisherCode(adUnitCode: string, pendingCode: PendingPublisherCode): void {
  pendingPublisherCodes.delete(adUnitCode);
  pendingPublisherCodes.set(adUnitCode, pendingCode);

  if (pendingPublisherCodes.size > MAX_PENDING_PUBLISHER_BIDS) {
    const oldestCode = pendingPublisherCodes.keys().next().value;
    if (oldestCode !== undefined) removePendingPublisherBidsForCode(oldestCode);
  }
}

/** Store an auction-local bid ID for precise one-shot GPT delivery correlation. */
function storePendingPublisherBid(adId: string, pendingBid: PendingPublisherBid): void {
  pendingPublisherBids.delete(adId);
  pendingPublisherBids.set(adId, pendingBid);

  if (pendingPublisherBids.size > MAX_PENDING_PUBLISHER_BIDS) {
    const oldestAdId = pendingPublisherBids.keys().next().value;
    if (oldestAdId !== undefined) pendingPublisherBids.delete(oldestAdId);
  }
}

/** Register every requested publisher code and any bid IDs returned for that auction. */
function registerPendingPublisherBids(
  publisherAdUnitCodes: Set<string>,
  bidResponses: unknown
): number {
  prunePendingPublisherBids();
  const registrationId = ++pendingPublisherRegistrationId;
  const expiresAt = Date.now() + PENDING_PUBLISHER_DELIVERY_TTL_MS;

  for (const adUnitCode of publisherAdUnitCodes) {
    removePendingPublisherBidsForCode(adUnitCode);
    storePendingPublisherCode(adUnitCode, { expiresAt, registrationId });
  }

  if (!bidResponses || typeof bidResponses !== 'object' || Array.isArray(bidResponses)) {
    return registrationId;
  }

  for (const [responseCode, responseGroup] of Object.entries(bidResponses)) {
    if (!responseGroup || typeof responseGroup !== 'object') continue;
    const bids = (responseGroup as { bids?: unknown }).bids;
    if (!Array.isArray(bids)) continue;

    for (const bid of bids) {
      if (!bid || typeof bid !== 'object') continue;
      const response = bid as { adId?: unknown; adUnitCode?: unknown };
      const adId = typeof response.adId === 'string' ? response.adId : undefined;
      const adUnitCode =
        typeof response.adUnitCode === 'string' ? response.adUnitCode : responseCode;
      if (!adId || !adUnitCode || !publisherAdUnitCodes.has(adUnitCode)) continue;

      storePendingPublisherBid(adId, { adUnitCode, expiresAt, registrationId });
    }
  }

  return registrationId;
}

/**
 * Partition slots by whether they belong to a pending publisher auction.
 *
 * A current `hb_adid` is the precise signal. When publishers intentionally
 * omit that targeting, a short-lived requested-code match preserves delivery
 * for no-bid and custom-targeting auctions. Without an ID, that fallback cannot
 * distinguish a delayed delivery from the first independent refresh, so it may
 * conservatively suppress one auction before its one-shot state is consumed.
 * A non-empty unmatched ID remains independent so stale targeting cannot
 * suppress a fresh auction. Every match is consumed once.
 */
function publisherDeliverySlots(targetSlots: RefreshGptSlot[]): Set<RefreshGptSlot> {
  prunePendingPublisherBids();
  const deliverySlots = new Set<RefreshGptSlot>();
  const deliveredCodes = new Set<string>();

  for (const slot of targetSlots) {
    const adIds = slot.getTargeting?.('hb_adid');
    const pendingBid = Array.isArray(adIds)
      ? adIds
          .filter((adId): adId is string => typeof adId === 'string' && adId.length > 0)
          .map((adId) => pendingPublisherBids.get(adId))
          .find((bid): bid is PendingPublisherBid => bid !== undefined)
      : undefined;
    const hasAdId =
      Array.isArray(adIds) && adIds.some((adId) => typeof adId === 'string' && adId.length > 0);
    const injectedSlot = findInjectedSlotForRefresh(slot);
    const pendingCode = hasAdId
      ? undefined
      : [refreshSlotElementId(slot), injectedSlot?.div_id]
          .filter((code): code is string => typeof code === 'string' && code.length > 0)
          .find((code) => pendingPublisherCodes.has(code));
    const adUnitCode = pendingBid?.adUnitCode ?? pendingCode;
    if (!adUnitCode) continue;

    deliverySlots.add(slot);
    deliveredCodes.add(adUnitCode);
  }

  deliveredCodes.forEach((adUnitCode) => removePendingPublisherBidsForCode(adUnitCode));
  return deliverySlots;
}

/** Evict publisher state after Prebid removes one or more ad units. */
function removePublisherState(adUnitCode?: string | string[]): void {
  if (!adUnitCode) {
    publisherAdUnitSnapshots.clear();
    pendingPublisherBids.clear();
    pendingPublisherCodes.clear();
    return;
  }

  const adUnitCodes = Array.isArray(adUnitCode) ? adUnitCode : [adUnitCode];
  for (const code of adUnitCodes) {
    publisherAdUnitSnapshots.delete(code);
    removePendingPublisherBidsForCode(code);
  }
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
 *
 * Idempotent per page: a `window.__tsjsPrebidShimInstalled` sentinel makes
 * repeat calls (double script inclusion, a bundle that still carries a
 * baked-in shim) a no-op instead of a double adapter registration.
 */
export function installPrebidNpm(config?: Partial<PrebidNpmConfig>): typeof pbjs {
  // The prebid integration requires the external Prebid.js bundle
  // (integrations.prebid.external_bundle_url). When it failed to load (network
  // error, SRI mismatch) window.pbjs is still the head-injected stub with no
  // API — installing the adapter is impossible, so bail out loudly.
  if (!hasPrebidJsApi()) {
    log.error(
      '[tsjs-prebid] window.pbjs has no Prebid.js API — the external Prebid bundle ' +
        'failed to load. Prebid integration disabled.'
    );
    return pbjs;
  }

  const sentinelWindow =
    typeof window === 'undefined' ? undefined : (window as { __tsjsPrebidShimInstalled?: boolean });
  if (sentinelWindow?.__tsjsPrebidShimInstalled) {
    return pbjs;
  }
  if (sentinelWindow) {
    sentinelWindow.__tsjsPrebidShimInstalled = true;
  }

  warnedMissingUserIdManifest = false;
  publisherAdUnitSnapshots = new Map();
  pendingPublisherBids = new Map();
  pendingPublisherCodes = new Map();
  pendingPublisherRegistrationId = 0;
  syntheticRefreshAdUnits = new WeakSet();

  const prebidWithRemoveAdUnit = pbjs as unknown as PrebidWithRemoveAdUnit;
  if (!prebidWithRemoveAdUnit.__tsRemoveAdUnitWrapped) {
    const originalRemoveAdUnit = prebidWithRemoveAdUnit.removeAdUnit;
    if (typeof originalRemoveAdUnit === 'function') {
      prebidWithRemoveAdUnit.removeAdUnit = function (adUnitCode?: string | string[]) {
        const result = originalRemoveAdUnit.call(this, adUnitCode);
        removePublisherState(adUnitCode);
        return result;
      };
      prebidWithRemoveAdUnit.__tsRemoveAdUnitWrapped = true;
    }
  }

  const injected = getInjectedConfig();
  const merged: PrebidNpmConfig = {
    endpoint: config?.endpoint,
    timeout: config?.timeout ?? injected?.timeout,
    debug: config?.debug ?? injected?.debug,
  };

  auctionEndpoint = merged.endpoint ?? '/auction';

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

    const opts = { ...(requestObj ?? {}) };
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const adUnits = ((opts as any).adUnits || pbjs.adUnits || []) as TrustedServerAdUnit[];
    const isSyntheticRefresh =
      adUnits.length > 0 && adUnits.every((unit) => syntheticRefreshAdUnits.has(unit));
    const publisherAdUnitCodes = new Set(
      adUnits
        .filter((unit) => !syntheticRefreshAdUnits.has(unit))
        .map((unit) => unit.code)
        .filter((code): code is string => typeof code === 'string' && code.length > 0)
    );

    // Ensure every ad unit has a trustedServer bid entry
    for (const unit of adUnits) {
      if (!syntheticRefreshAdUnits.has(unit)) {
        const snapshot = capturePublisherAdUnitSnapshot(unit, clientSideBidders);
        if (snapshot && unit.code) {
          storePublisherAdUnitSnapshot(unit.code, snapshot);
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
      const registrationId = isSyntheticRefresh
        ? undefined
        : registerPendingPublisherBids(publisherAdUnitCodes, args[0]);
      if (typeof originalBidsBack !== 'function') return;

      try {
        originalBidsBack.apply(this, args as Parameters<typeof originalBidsBack>);
      } catch (error) {
        if (registrationId !== undefined) {
          publisherAdUnitCodes.forEach((code) =>
            removePendingPublisherBidsForCode(code, registrationId)
          );
        }
        throw error;
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

  // Validate that every client-side bidder has its adapter compiled into the
  // external Prebid.js bundle. The bundle stamps the registered bidder codes
  // (including aliases such as adform/adformOpenRTB for the adf module) on
  // window.__tsjs_prebid_bundle; a missing code means the bidder was listed
  // in client_side_bidders but not included in the generated bundle, so it is
  // silently dropped from both server-side and client-side auctions. Fall
  // back to the module-name list for bundles stamped before bidderCodes.
  const manifest = getExternalBundleManifest();
  const bundledBidderCodes = manifest?.bidderCodes ?? manifest?.adapters;
  if (bundledBidderCodes === undefined) {
    if (clientSideBidders.size > 0) {
      log.warn(
        '[tsjs-prebid] external Prebid bundle did not stamp an adapter manifest; ' +
          'cannot verify client_side_bidders adapters'
      );
    }
  } else {
    for (const bidder of clientSideBidders) {
      if (!bundledBidderCodes.includes(bidder)) {
        log.error(
          `[tsjs-prebid] client-side bidder "${bidder}" has no adapter in the external ` +
            'Prebid bundle. Add its adapter to [integrations.prebid.bundle].adapters in ' +
            'trusted-server.toml and rebuild it with `ts prebid bundle`.'
        );
      }
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

      if (!targetSlots.length || (slots !== undefined && targetSlots.length !== slots.length)) {
        return originalRefresh(slots, opts);
      }

      const deliverySlots = publisherDeliverySlots(targetSlots);
      const independentSlots = targetSlots.filter((slot) => !deliverySlots.has(slot));
      if (independentSlots.length === 0) {
        recordPrebidRefreshForDiagnostics(targetSlots);
        return dispatchPrebidRefresh(originalRefresh, slots, opts);
      }

      independentSlots.forEach(clearRefreshTargeting);

      const adUnits = independentSlots.map((slot) => {
        const injectedSlot = findInjectedSlotForRefresh(slot);
        const code = refreshSlotElementId(slot) ?? 'refresh-slot';
        // A TS-owned slot may be defined on `${div_id}-container`, so the GPT
        // element id used as the synthetic refresh code can differ from the
        // inner `div_id` the publisher keyed their ad unit by. Recover from both.
        const candidateCodes = [code, injectedSlot?.div_id];
        const zone =
          injectedSlot?.targeting?.[ZONE_KEY] ??
          firstTargetingValue(slot.getTargeting?.(ZONE_KEY)) ??
          publisherZoneForRefresh(candidateCodes);
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

      // Preserve GPT Single Request Architecture: when a publisher refresh
      // includes both already-targeted delivery slots and independent slots,
      // delay the whole original list until the independent auction completes.
      // A one-shot fallback prevents a failed Prebid callback from dropping any
      // slots, and a late callback cannot issue a second GAM request.
      let completed = false;
      let fallbackTimer: ReturnType<typeof setTimeout> | undefined;
      function completeRefresh(applyTargeting: boolean): void {
        if (completed) return;
        completed = true;
        if (fallbackTimer !== undefined) clearTimeout(fallbackTimer);
        if (applyTargeting) {
          try {
            pbjs.setTargetingForGPTAsync?.(refreshAdUnitCodes);
          } catch (error) {
            log.error('[tsjs-prebid] refresh targeting failed', error);
          }
        }
        recordPrebidRefreshForDiagnostics(targetSlots);
        dispatchPrebidRefresh(originalRefresh, slots, opts);
      }

      try {
        pbjs.requestBids({
          adUnits,
          bidsBackHandler: () => completeRefresh(true),
          timeout: timeoutMs,
        });
        // A one-shot watchdog completes the GAM request even if Prebid never
        // invokes its callback. Apply any bids available at that point before
        // refreshing because Prebid's own timeout completion may run later.
        if (!completed) {
          fallbackTimer = setTimeout(() => completeRefresh(true), timeoutMs);
        }
      } catch (error) {
        log.error('[tsjs-prebid] refresh auction failed', error);
        completeRefresh(false);
      }
    };

    log.info('[tsjs-prebid] GPT refresh handler installed');
  });
}

/**
 * Configure identity sync behavior for the generated Prebid User ID modules.
 *
 * The external bundle generator statically imports the selected modules into
 * its generated entry. This post-window-load configuration controls when
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
  // When the external bundle failed to load, installPrebidNpm bailed out and
  // pbjs.requestBids is undefined. Installing the refresh handler anyway
  // would clear TS-applied GPT targeting on every publisher refresh and then
  // fail to run the replacement auction — leave GPT untouched instead.
  if (hasPrebidJsApi()) {
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
}

export { pbjs };
export default installPrebidNpm;
