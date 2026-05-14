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
import 'prebid.js/modules/consentManagementTcf.js';
import 'prebid.js/modules/consentManagementGpp.js';
import 'prebid.js/modules/consentManagementUsp.js';
import 'prebid.js/modules/userId.js';

// Client-side bid adapters — self-register with prebid.js on import.
// The set of adapters is controlled by the TSJS_PREBID_ADAPTERS env var at
// build time. See _adapters.generated.ts (written by build-all.mjs).
// User ID submodules come from the deterministic attested preset in
// user_id_modules.json. See _user_ids.generated.ts.
// When a bidder is listed in `client_side_bidders` in trusted-server.toml,
// the requestBids shim leaves its bids untouched and the corresponding
// adapter handles them natively in the browser.
import './_adapters.generated';
import './_user_ids.generated';

import { log } from '../../core/log';
import { buildAdRequest, parseAuctionResponse } from '../../core/auction';
import type { AuctionBid, AuctionEid } from '../../core/auction';
import { DEFAULT_PREBID_USER_ID_MODULES, PREBID_USER_ID_MODULE_REGISTRY } from './user_id_modules';

const ADAPTER_CODE = 'trustedServer';
const BIDDER_PARAMS_KEY = 'bidderParams';
const ZONE_KEY = 'zone';

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

function recordUserIdModuleDiagnostics(): PrebidUserIdDiagnostics {
  const configuredUserIdNames = [...new Set(readConfiguredUserIdNames())].sort();
  const coveredConfigNames = new Set(
    PREBID_USER_ID_MODULE_REGISTRY.filter((entry) =>
      DEFAULT_PREBID_USER_ID_MODULES.includes(entry.moduleName)
    ).flatMap((entry) => entry.configNames)
  );
  const missingConfiguredUserIdNames = configuredUserIdNames.filter(
    (name) => !coveredConfigNames.has(name)
  );

  const diagnostics: PrebidUserIdDiagnostics = {
    includedModules: [...DEFAULT_PREBID_USER_ID_MODULES],
    configuredUserIdNames,
    missingConfiguredUserIdNames,
  };

  if (typeof window !== 'undefined') {
    const tsjsWindow = window as typeof window & {
      __tsjs_prebid_diagnostics?: { userIdModules?: PrebidUserIdDiagnostics };
    };
    tsjsWindow.__tsjs_prebid_diagnostics = {
      ...(tsjsWindow.__tsjs_prebid_diagnostics ?? {}),
      userIdModules: diagnostics,
    };
  }

  for (const name of missingConfiguredUserIdNames) {
    log.warn(`[tsjs-prebid] configured User ID module "${name}" is not included in TSJS`);
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
type TrustedServerAdUnit = { code?: string; bids?: TrustedServerBid[] };
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

function sanitizeAuctionUid(uid: {
  id?: unknown;
  atype?: unknown;
  ext?: unknown;
}): AuctionEid['uids'][number] | undefined {
  if (typeof uid?.id !== 'string' || uid.id.length === 0) {
    return undefined;
  }

  const sanitizedUid: AuctionEid['uids'][number] = { id: uid.id };

  if (Number.isInteger(uid.atype) && uid.atype >= 0 && uid.atype <= 255) {
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
export function installPrebidNpm(config?: Partial<PrebidNpmConfig>): typeof pbjs {
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

    const opts = requestObj || {};
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const adUnits = ((opts as any).adUnits || pbjs.adUnits || []) as TrustedServerAdUnit[];

    // Ensure every ad unit has a trustedServer bid entry
    for (const unit of adUnits) {
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

      const tsParams: Record<string, unknown> = {
        [BIDDER_PARAMS_KEY]: bidderParams,
        ...(zone ? { [ZONE_KEY]: zone } : {}),
      };
      const existingTsBid = unit.bids.find((b) => b.bidder === ADAPTER_CODE);
      if (existingTsBid) {
        const paramsWithoutZone = {
          ...(existingTsBid.params ?? {}),
        };
        delete paramsWithoutZone[ZONE_KEY];

        existingTsBid.params = {
          ...paramsWithoutZone,
          ...tsParams,
        };
      } else {
        unit.bids.push({ bidder: ADAPTER_CODE, params: tsParams });
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
      if (typeof originalBidsBack === 'function') {
        originalBidsBack.apply(this, args);
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

  // Validate that every client-side bidder has its adapter registered.
  // Adapters self-register on import, so a missing adapter means the bidder
  // was listed in client_side_bidders but not in TSJS_PREBID_ADAPTERS at
  // build time. Without the adapter the bidder is silently dropped from both
  // server-side and client-side auctions.
  for (const bidder of clientSideBidders) {
    try {
      if (!adapterManager.getBidAdapter(bidder)) {
        log.error(
          `[tsjs-prebid] client-side bidder "${bidder}" has no adapter loaded. ` +
            `Add it to TSJS_PREBID_ADAPTERS at build time.`
        );
      }
    } catch {
      log.error(
        `[tsjs-prebid] client-side bidder "${bidder}" has no adapter loaded. ` +
          `Add it to TSJS_PREBID_ADAPTERS at build time.`
      );
    }
  }

  log.info('[tsjs-prebid] prebid initialized with trustedServer adapter');

  return pbjs;
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
}

export { pbjs };
export default installPrebidNpm;
