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

// Client-side bid adapters — self-register with prebid.js on import.
// The set of adapters is controlled by the TSJS_PREBID_ADAPTERS env var at
// build time. See _adapters.generated.ts (written by build-all.mjs).
// When a bidder is listed in `client_side_bidders` in trusted-server.toml,
// the requestBids shim leaves its bids untouched and the corresponding
// adapter handles them natively in the browser.
import './_adapters.generated';

import { log } from '../../core/log';
import { buildAdRequest, parseAuctionResponse } from '../../core/auction';
import type { AuctionBid } from '../../core/auction';

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
      const payload = buildAdRequest(validBidRequests);
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

      // Remove server-side bidder entries — they are now handled via the
      // trustedServer adapter. Only keep client-side bidders (which run via
      // their native Prebid.js adapters) and the trustedServer bid itself.
      unit.bids = unit.bids.filter(
        (b) => b.bidder === ADAPTER_CODE || clientSideBidders.has(b.bidder ?? '')
      );
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

// Self-initialize when loaded in a browser (same pattern as other integrations).
if (typeof window !== 'undefined') {
  installPrebidNpm();
}

export { pbjs };
export default installPrebidNpm;

// ─── TS pbRender bridge ────────────────────────────────────────────────────
// NOTE: The bridge implementation lives in gpt/index.ts (installTsRenderBridge)
// to avoid pulling the full Prebid bundle into tsjs-gpt.js via
// inlineDynamicImports. The export below is kept for direct use by the Prebid
// bundle when slim-Prebid ships in Phase B.

/** Minimal display renderer — set as `window.render` inside the ad iframe. */
const TS_DISPLAY_RENDERER = `(function(){window.render=function(d,h,w){var doc=w.document;var f=h.mkFrame(doc,{width:d.width||'100%',height:d.height||'100%'});if(d.adUrl&&!d.ad){f.src=d.adUrl;}else{f.srcdoc=d.ad;}doc.body.appendChild(f);};})();`;

/**
 * Install the TS → pbRender bridge.
 *
 * Listens for Prebid cross-domain `"Prebid Request"` messages on the publisher
 * window. When the `adId` matches a TS server-side bid that has PBS Cache
 * coordinates (`hb_cache_host` / `hb_cache_path`), the bridge fetches the
 * creative markup from PBS Cache and replies with a `"Prebid Response"` so the
 * GAM Prebid creative can render without needing Prebid.js's local bid store.
 *
 * Must be called after `installPrebidNpm()` so `window.__pb_locator__` is set
 * and pbRender can locate this window.
 */
export function installTsRenderBridge(): void {
  if (typeof window === 'undefined') return;

  window.addEventListener('message', async (e: MessageEvent) => {
    let data: Record<string, unknown>;
    try {
      data = typeof e.data === 'object' ? e.data : (JSON.parse(e.data as string) as Record<string, unknown>);
    } catch {
      return;
    }

    if (data['message'] !== 'Prebid Request') return;
    const adId = data['adId'] as string | undefined;
    if (!adId) return;

    // Only the first port is used — it's channel.port2 from the creative's
    // MessageChannel; posting to it triggers channel.port1.onmessage.
    const port = e.ports?.[0];
    if (!port) return;

    // Build reverse map: hb_adid → slotId from live window.tsjs.bids.
    // Read live so SPA navigation updates are captured automatically.
    const bids = window.tsjs?.bids ?? {};
    let slotId: string | undefined;
    let matchedBid: typeof bids[string] | undefined;
    for (const [sid, bid] of Object.entries(bids)) {
      if (bid.hb_adid === adId) {
        slotId = sid;
        matchedBid = bid;
        break;
      }
    }

    if (!slotId || !matchedBid?.hb_cache_host || !matchedBid?.hb_cache_path) {
      // Not a TS bid — let Prebid.js handle it via its own listener.
      return;
    }

    // Fetch ad markup from PBS Cache.
    const cacheUrl = `https://${matchedBid.hb_cache_host}${matchedBid.hb_cache_path}?uuid=${encodeURIComponent(adId)}`;
    let ad: string;
    try {
      const res = await fetch(cacheUrl, { mode: 'cors' });
      if (!res.ok) return;
      ad = await res.text();
    } catch {
      return;
    }

    // Look up dimensions from window.tsjs.adSlots.
    const slot = window.tsjs?.adSlots?.find((s) => s.id === slotId);
    const [width, height] = slot?.formats?.[0] ?? [728, 90];

    // Reply with the Prebid Response contract that crossDomain.js expects.
    port.postMessage(
      JSON.stringify({
        message: 'Prebid Response',
        adId,
        ad,
        renderer: TS_DISPLAY_RENDERER,
        width,
        height,
      }),
    );

    log.debug(`[tsjs-prebid] pbRender bridge served ad for slot '${slotId}' from PBS Cache`);
  });

  log.info('[tsjs-prebid] TS pbRender bridge installed');
}
