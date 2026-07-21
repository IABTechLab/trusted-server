// Shared TypeScript types for the tsjs core API and extensions.
export type Size = readonly [number, number];

export interface Banner {
  sizes: ReadonlyArray<Size>;
}

export interface MediaTypes {
  banner?: Banner;
}

export interface Bid {
  bidder: string;
  params?: Record<string, unknown>;
}

export interface AdUnit {
  code: string;
  mediaTypes?: MediaTypes;
  bids?: Bid[];
}

/** Minimal shape of a server-side auction slot injected into `window.tsjs.adSlots`. */
export interface AuctionSlot {
  id: string;
  gam_unit_path: string;
  div_id: string;
  formats: Array<[number, number]>;
  targeting?: Record<string, string>;
}

/** Debug-only copy of server-side bid fields exposed for pipeline inspection. */
export interface AuctionDebugBidData {
  slot_id?: string;
  price?: number | null;
  currency?: string;
  creative?: string | null;
  adomain?: string[] | null;
  bidder?: string;
  width?: number;
  height?: number;
  nurl?: string | null;
  burl?: string | null;
  bid_id?: string | null;
  ad_id?: string | null;
  creative_id?: string | null;
  cache_id?: string | null;
  cache_host?: string | null;
  cache_path?: string | null;
  metadata?: Record<string, unknown>;
}

export type ApsTagType = 'iframe' | 'script';

/** Version 1 Trusted Server APS renderer descriptor. */
export interface ApsRendererV1 {
  type: 'aps';
  version: 1;
  accountId: string;
  bidId: string;
  creativeId?: string;
  tagType: ApsTagType;
  creativeUrl: string;
  aaxResponse: string;
  width: number;
  height: number;
}

export type AuctionBidRenderer = ApsRendererV1;

/** A client-side Prebid bid's generated ad ID bound to its APS render capability. */
export interface ApsPrebidRendererEntry {
  adUnitCode: string;
  renderer: ApsRendererV1;
  registeredAt: number;
  expiresAt: number;
  /** Notify Prebid that GAM selected this bid before replying to Universal Creative. */
  markWinner(): void;
  /** Mark Prebid's bid rendered after the Universal Creative response is posted. */
  markRendered(): void;
}

/** Bid targeting data from the server-side auction, injected into `window.tsjs.bids`. */
export interface AuctionBidData {
  hb_pb?: string;
  hb_bidder?: string;
  hb_adid?: string;
  hb_cache_host?: string;
  hb_cache_path?: string;
  /** Server-side auction ID — trace key joining this bid to server logs. */
  hb_auction_id?: string;
  /** Upstream creative ID (OpenRTB `crid`), when the bidder returned one. */
  hb_crid?: string;
  /** Trace hash of the bid's raw creative markup (16 hex chars of SHA-256). */
  hb_adm_hash?: string;
  /** Winning creative width; the bridge sizes the inline render from this. */
  w?: number;
  /** Winning creative height; the bridge sizes the inline render from this. */
  h?: number;
  nurl?: string;
  burl?: string;
  /** Typed winning-bid renderer capability. */
  renderer?: AuctionBidRenderer;
  /**
   * Sanitized winning creative markup for local rendering through the pbRender
   * bridge. Present whenever the winning bid carried a creative that passed the
   * server-side sanitize/rewrite boundary; absent when there was no creative or
   * it was rejected (e.g. over the 1 MiB cap), in which case the bridge falls
   * back to the PBS Cache coordinates. This is NOT gated by
   * `inject_adm_for_testing`.
   */
  adm?: string;
  /**
   * Verbose per-bid debug blob (carries the raw, un-sanitized creative among
   * other fields). Only present when `[debug] inject_adm_for_testing = true`;
   * its presence is also the client-side gate for the testing-only direct
   * GAM-replace path.
   */
  debug_bid?: AuctionDebugBidData;
}

/** How a creative reached the page for a [`RenderRecord`]. */
export type RenderServedFrom = 'inline' | 'gam' | 'debug-adm' | 'pbs-cache';

/**
 * One entry in `window.tsjs.renders` — the client-side half of the render
 * trace. Field values mirror the server-side `auction winner:` log line so
 * the two can be joined on (auctionId, slotId).
 */
export interface RenderRecord {
  /** Slot the creative was rendered for. */
  slotId: string;
  /** Which render path produced this record. */
  path: 'auction' | 'ssat';
  /** Whether a creative actually rendered (false for empty/rejected). */
  rendered: boolean;
  /** Actual DOM element ID the slot resolved to (div_id may be a prefix). */
  elementId?: string;
  /** Server-side auction ID. */
  auctionId?: string;
  /** Winning bidder / seat. */
  bidder?: string;
  /** hb_adid (PBS cache UUID or OpenRTB adid). */
  adId?: string;
  /** Upstream creative ID (OpenRTB crid). */
  creativeId?: string;
  /** Trace hash of the creative markup (16 hex chars of SHA-256). */
  admHash?: string;
  /** Mechanism that delivered the creative. */
  servedFrom?: RenderServedFrom;
  /** How many renders this slot has seen (SPA navigations, refreshes). */
  count: number;
  /** Epoch ms when the record was written. */
  at: number;
}

export interface TsjsApi {
  version: string;
  que: Array<() => void>;
  addAdUnits(units: AdUnit | AdUnit[]): void;
  renderAdUnit(codeOrUnit: string | AdUnit): void;
  renderAllAdUnits(): void;
  setConfig?(cfg: Record<string, unknown>): void;
  getConfig?(): Record<string, unknown>;
  requestAds?(opts?: { bidsBackHandler?: () => void; timeout?: number }): void;
  requestAds?(
    callback: () => void,
    opts?: { bidsBackHandler?: () => void; timeout?: number }
  ): void;
  log?: {
    setLevel(l: 'silent' | 'error' | 'warn' | 'info' | 'debug'): void;
    getLevel(): 'silent' | 'error' | 'warn' | 'info' | 'debug';
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
    debug(...args: unknown[]): void;
  };

  // ── Server-side auction runtime (populated by TS edge injection) ──────────
  /** Ad slot definitions injected at <head> open. */
  adSlots?: AuctionSlot[];
  /** Winning bid targeting data injected before </body>. */
  bids?: Record<string, AuctionBidData>;
  /**
   * Bounded client-side Prebid APS renderer capabilities keyed by Prebid's generated
   * `hb_adid`. The Universal Creative bridge consumes each entry at most once.
   */
  apsPrebidRenderers?: Record<string, ApsPrebidRendererEntry>;
  /** Initialises GPT slots with server-side bid targeting and calls refresh(). */
  adInit?: () => void;
  /** Render-trace registry: latest render per slot (see [`RenderRecord`]). */
  renders?: Record<string, RenderRecord>;
  /** GPT slot objects TS defined — used to destroy stale slots on SPA navigation. */
  prevGptSlots?: unknown[];
  /** Guards one-time-per-page enableSingleRequest/enableServices calls. */
  servicesEnabled?: boolean;
  /** Maps actualDivId → slotId for slotRenderEnded billing lookup. */
  divToSlotId?: Record<string, string>;
  /**
   * Win/billing beacons already fired, keyed by `slotId|bidIdentity|kind|url`.
   * Used by the GPT render bridge so a bid's nurl/burl fire at most once even
   * across repeated Prebid Universal Creative requests for the same adId.
   */
  firedBeacons?: Record<string, boolean>;
  /** Slot-level GPT targeting keys TS applied on the previous route. */
  prevSlotTargetingKeys?: Record<string, string[]>;
  /**
   * One-shot bypass for the slim-Prebid refresh wrapper: true only while
   * adInit() runs its internal refresh of server-side-targeted slots, so the
   * wrapper passes that refresh straight to GPT instead of starting a
   * client-side auction that would clear the just-applied TS targeting.
   */
  adInitRefreshInProgress?: boolean;
  /**
   * True once the publisher has called `googletag.pubads().disableInitialLoad()`.
   * GPT exposes no getter for this state, so it is tracked by wrapping the
   * setter. When set, `display()` only registers a slot and the ad request must
   * come from a `refresh()`; adInit() uses this to refresh its own freshly
   * defined slots so they are not left blank.
   */
  gptInitialLoadDisabled?: boolean;
  /** Guards SPA pushState hook installation. */
  spaHookInstalled?: boolean;
}
