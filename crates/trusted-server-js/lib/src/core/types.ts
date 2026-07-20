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
  ad_id?: string | null;
  bid_id?: string | null;
  crid?: string | null;
  cache_id?: string | null;
  cache_host?: string | null;
  cache_path?: string | null;
  metadata?: Record<string, unknown>;
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
  nurl?: string;
  burl?: string;
  /** Raw creative markup. Only present when `[debug] inject_adm_for_testing = true`. */
  adm?: string;
  /** Debug-only bid field mirror. Only present when `[debug] inject_adm_for_testing = true`. */
  debug_bid?: AuctionDebugBidData;
}

/** How a creative reached the page for a [`RenderRecord`]. */
export type RenderServedFrom = 'inline' | 'gam' | 'debug-adm' | 'pbs-cache' | 'prebid';

/**
 * One entry in `window.tsjs.renders` — the client-side half of the render
 * trace. Field values mirror the server-side `auction winner:` log line so
 * the two can be joined on (auctionId, slotId).
 */
export interface RenderRecord {
  /** Slot the creative was rendered for. */
  slotId: string;
  /**
   * Which render path produced this record.
   *
   * `ssat` is claimed only for the render that consumes the server-side
   * targeting TS just applied — the server-side auction runs once per
   * navigation, so a later GAM refresh of the same slot is NOT an SSAT render
   * even though `window.tsjs.bids` still holds that auction's data.
   * `gam-refresh` is that later render: GAM re-requested the slot and TS cannot
   * attribute the returned creative to any TS auction.
   */
  path: 'auction' | 'ssat' | 'gam-refresh';
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
  /**
   * GAM's own `slotRenderEnded.isEmpty` (SSAT/GAM path only). `true` means GAM
   * itself reported the slot empty. Undefined on the `/auction` path, which
   * never involves GAM.
   */
  gamEmpty?: boolean;
  /**
   * Whether Trusted Server actually placed the creative markup itself:
   * `true` for the `/auction` iframe render and for a synchronous
   * `injectAdmIntoSlot` placement; `false` when TS only applied GAM targeting
   * (prod GAM path — the creative, if any, is GAM's and lives in a cross-origin
   * iframe TS cannot read); `undefined` when placement was deferred/unknown.
   *
   * This is the honest "is it TS's creative" signal — distinct from `rendered`
   * (GAM said something rendered) and `visible` (the slot box is on-screen).
   */
  injected?: boolean;
  /**
   * Whether the slot element was effectively visible at record time — non-zero
   * box and no ancestor `display:none` / `visibility:hidden` / `opacity:0`.
   * Catches slots that "rendered" but are hidden behind a publisher reveal gate.
   */
  visible?: boolean;
  /** How many renders this slot has seen (SPA navigations, refreshes). */
  count: number;
  /**
   * Page-global render sequence, starting at 1 and shared by the trace panel
   * row and the on-creative badge. Unlike `count` (per-slot) this is unique
   * across the page, so a badge reading `#12` identifies exactly one row.
   */
  seq: number;
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
  /** Initialises GPT slots with server-side bid targeting and calls refresh(). */
  adInit?: () => void;
  /** Render-trace registry: latest render per slot (see [`RenderRecord`]). */
  renders?: Record<string, RenderRecord>;
  /**
   * Append-only history of every render, oldest first, bounded to the most
   * recent entries. `renders` collapses to one row per slot (useful for
   * "did this slot ever render" checks); this keeps each individual render so
   * a refreshing page shows a timeline instead of a climbing counter.
   */
  renderLog?: RenderRecord[];
  /** GPT slot objects TS defined — used to destroy stale slots on SPA navigation. */
  prevGptSlots?: unknown[];
  /** Guards one-time-per-page enableSingleRequest/enableServices calls. */
  servicesEnabled?: boolean;
  /** Maps actualDivId → slotId for slotRenderEnded billing lookup. */
  divToSlotId?: Record<string, string>;
  /**
   * Per-slot flag: TS applied server-side bid targeting and no GAM render has
   * consumed it yet. Set by `adInit()`, cleared by the first `slotRenderEnded`
   * for that slot, so only that render is attributed to the SSAT auction (see
   * [`RenderRecord.path`]). Publisher-driven refreshes afterwards find it false
   * and are recorded as `gam-refresh` without the stale auction tuple.
   */
  ssatTargetingFresh?: Record<string, boolean>;
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
