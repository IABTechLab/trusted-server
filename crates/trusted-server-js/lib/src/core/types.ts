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
   * True once the publisher has disabled GPT initial load through
   * `googletag.setConfig()` or `googletag.pubads().disableInitialLoad()`.
   * GPT exposes no getter for this state, so TS tracks both configuration APIs.
   * When set, `display()` only registers a slot and the ad request must come
   * from a `refresh()`; adInit() uses this to refresh its own freshly defined
   * slots so they are not left blank.
   */
  gptInitialLoadDisabled?: boolean;
  /** Guards SPA pushState hook installation. */
  spaHookInstalled?: boolean;
}
