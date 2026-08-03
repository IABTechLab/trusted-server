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
  /** Trace-only OpenRTB bid identifier. */
  hb_bid_id?: string;
  /** Trace-only server-side auction identifier. */
  hb_auction_id?: string;
  /** Trace-only OpenRTB creative identifier. */
  hb_crid?: string;
  /** Trace hash of delivered creative markup. */
  hb_adm_hash?: string;
  nurl?: string;
  burl?: string;
  /** Typed winning-bid renderer capability. */
  renderer?: AuctionBidRenderer;
  /** Winning creative width used by the inline render bridge. */
  w?: number;
  /** Winning creative height used by the inline render bridge. */
  h?: number;
  /**
   * Sanitized winning creative markup for local rendering through the pbRender
   * bridge. Present whenever the winning bid carried a creative that passed the
   * server-side sanitize/rewrite boundary; absent when there was no creative or
   * it was rejected (e.g. over the 1 MiB cap), in which case the bridge falls
   * back to the PBS Cache coordinates. This is NOT gated by
   * `inject_adm_for_testing`.
   */
  adm?: string;
  /** Debug-only bid field mirror. Only present when `[debug] inject_adm_for_testing = true`. */
  debug_bid?: AuctionDebugBidData;
}

/** How a creative reached the page for a [`RenderRecord`]. */
export type RenderServedFrom = 'inline' | 'gam' | 'debug-adm' | 'pbs-cache' | 'prebid';

/** Client-side record joining a rendered creative to its auction winner. */
export interface RenderRecord {
  slotId: string;
  path: 'auction' | 'ssat' | 'gam-refresh';
  rendered: boolean;
  elementId?: string;
  auctionId?: string;
  bidder?: string;
  adId?: string;
  bidId?: string;
  creativeId?: string;
  admHash?: string;
  servedFrom?: RenderServedFrom;
  gamEmpty?: boolean;
  injected?: boolean;
  visible?: boolean;
  count: number;
  seq: number;
  at: number;
}

/**
 * Lifecycle state for a GPT slot TS created before its publisher declares it.
 *
 * Stored on `window.tsjs` so the head bootstrap and the full TSJS bundle share
 * one handoff protocol.
 */
export interface GptSlotHandoff {
  gamUnitPath: string;
  formats: Array<[number, number]>;
  /** Stable configured prefix used to safely bridge framework-generated IDs. */
  divIdPrefix: string;
  /** Element ID GPT received when TS created the fallback slot. */
  slotElementId: string;
  publisherClaimed: boolean;
  suppressPublisherDisplay: boolean;
  suppressPublisherRefresh: boolean;
}

export type GptDiagnosticsCallbackKind =
  | 'slotRequested'
  | 'slotResponseReceived'
  | 'slotRenderEnded'
  | 'slotOnload'
  | 'impressionViewable'
  | 'slotVisibilityChanged';

export type GptDiagnosticsCallbackDisposition = 'matched' | 'unmatched' | 'ambiguous';

export type GptDiagnosticsBindingReason =
  | 'missing_slot_element_id'
  | 'missing_element'
  | 'duplicate_dom_id'
  | 'dom_uniqueness_unverifiable'
  | 'duplicate_gpt_slot_id';

export interface GptDiagnosticsBinding {
  status: 'bound' | 'unbound' | 'ambiguous';
  reason?: GptDiagnosticsBindingReason;
}

export interface GptDiagnosticsDurations {
  requestToResponseMs?: number;
  responseToRenderMs?: number;
  requestToRenderMs?: number;
  renderToLoadMs?: number;
  renderToViewableMs?: number;
}

export interface GptDiagnosticsRequestCycle {
  requestNumber: number;
  requestedAtMs?: number;
  responseAtMs?: number;
  renderAtMs?: number;
  loadAtMs?: number;
  viewableAtMs?: number;
  durations: GptDiagnosticsDurations;
  isEmpty?: boolean;
  size?: Size;
  isBackfill?: boolean;
  slotContentChanged?: boolean;
  incompleteSequence: boolean;
}

export interface GptDiagnosticsSlotExport {
  runtimeSlotNumber: number;
  slotElementId?: string;
  adUnitPath?: string;
  binding: GptDiagnosticsBinding;
  currentVisibilityPercentage?: number;
  maximumVisibilityPercentage?: number;
  requests: GptDiagnosticsRequestCycle[];
}

export interface GptDiagnosticsCallbackIssue {
  kind: GptDiagnosticsCallbackKind;
  runtimeSlotNumber: number;
  slotElementId?: string;
  timestampMs: number;
  disposition: GptDiagnosticsCallbackDisposition;
  reason: string;
}

export interface GptDiagnosticsCoverageCounters {
  observed: number;
  matched: number;
  unmatched: number;
  ambiguous: number;
}

export interface GptDiagnosticsExportV1 {
  version: 1;
  capturedAt: string;
  page: {
    origin: string;
    pathname: string;
  };
  slots: GptDiagnosticsSlotExport[];
  callbackIssues: GptDiagnosticsCallbackIssue[];
  coverage: Record<GptDiagnosticsCallbackKind, GptDiagnosticsCoverageCounters>;
  metadata: {
    droppedCallbacks: number;
    evictedSlots: number;
    evictedRequestCycles: number;
  };
}

export interface GptDiagnosticsApi {
  snapshot(): GptDiagnosticsExportV1;
  export(): void;
  subscribe(listener: (snapshot: GptDiagnosticsExportV1) => void): () => void;
  show(): void;
  hide(): void;
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
  /** Render-trace registry: latest render per slot. */
  renders?: Record<string, RenderRecord>;
  /** Append-only history of every render. */
  renderLog?: RenderRecord[];
  /** Monotonic render generation for cancelling stale async work. */
  renderGeneration?: number;
  /** Page-global render sequence counter. */
  renderSeq?: number;
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
   * Whether the publisher disabled GPT initial load through
   * `googletag.setConfig()` or `googletag.pubads().disableInitialLoad()`.
   * TS synchronizes this from GPT's getter and wraps both configuration APIs as
   * a fallback when the getter is unavailable.
   * When set, `display()` only registers a slot and the ad request must come
   * from a `refresh()`; adInit() uses this to refresh its own freshly defined
   * slots so they are not left blank.
   */
  gptInitialLoadDisabled?: boolean;
  /** Late publisher claims for TS-created GPT slots, keyed by actual div ID. */
  gptSlotHandoffs?: Record<string, GptSlotHandoff>;
  /** True only while TS calls a GPT function that the handoff wrappers observe. */
  gptSlotHandoffInternal?: boolean;
  /** Guards SPA pushState hook installation. */
  spaHookInstalled?: boolean;
  /**
   * Monotonic count of committed SPA navigations, incremented synchronously by
   * the SPA auction hook the moment it accepts a route change. The deferred
   * initial-adInit bootstrap ([`scheduleInitialAdInit`]) is pinned to
   * generation 0 (the SSR document) and no-ops when a navigation has
   * committed — before it was called, or while it was pending. A counter is
   * used instead of a URL comparison so the guard cannot diverge from the
   * auction path: the hook's route identity is pathname plus query (matching
   * the page-bids refresh hook), hash-only changes leave the counter
   * unchanged, and an `/a → /b → /a` round trip (where the URL compares
   * equal again) still advances it.
   */
  navGeneration?: number;
  /**
   * Defers the initial `adInit()` until after React hydration: window `load`,
   * then a double `requestAnimationFrame`. Called by the server-injected
   * `</body>` bids script with the SSR bids payload. The whole initial pass
   * is pinned to navigation generation 0 (the SSR document): if an SPA
   * navigation has already committed — or commits while the deferred callback
   * is pending — the payload is dropped and `adInit()` is not run, so a stale
   * SSR bootstrap can neither clobber the live route's bids nor re-run it.
   * Lives in the bundle so the lifecycle is executable under test and shares
   * [`navGeneration`] with the SPA auction hook; `gpt_bootstrap.js` installs
   * a minimal fallback for pages where the bundle fails to load.
   */
  scheduleInitialAdInit?: (initialBids?: Record<string, AuctionBidData>) => void;
  /** Read-only GPT lifecycle diagnostics API, present only in an activated tab. */
  gptDiagnostics?: GptDiagnosticsApi;
}
