// Shared TypeScript types for the tsjs core API and extensions.
export type Size = readonly [number, number];

export interface Banner {
  sizes: ReadonlyArray<Size>;
}

export interface MediaTypes {
  banner?: Banner | undefined;
}

export interface Bid {
  bidder: string;
  params?: Record<string, unknown> | undefined;
}

export interface AdUnit {
  code: string;
  mediaTypes?: MediaTypes | undefined;
  bids?: Bid[] | undefined;
}

/** Minimal shape of a server-side auction slot injected into `window.tsjs.adSlots`. */
export interface AuctionSlot {
  id: string;
  gam_unit_path: string;
  div_id: string;
  formats: Array<[number, number]>;
  targeting?: Record<string, string> | undefined;
}

/** Debug-only copy of server-side bid fields exposed for pipeline inspection. */
export interface AuctionDebugBidData {
  slot_id?: string | undefined;
  price?: number | null | undefined;
  currency?: string | undefined;
  creative?: string | null | undefined;
  adomain?: string[] | null | undefined;
  bidder?: string | undefined;
  width?: number | undefined;
  height?: number | undefined;
  nurl?: string | null | undefined;
  burl?: string | null | undefined;
  bid_id?: string | null | undefined;
  ad_id?: string | null | undefined;
  creative_id?: string | null | undefined;
  cache_id?: string | null | undefined;
  cache_host?: string | null | undefined;
  cache_path?: string | null | undefined;
  metadata?: Record<string, unknown> | undefined;
}

export type ApsTagType = 'iframe' | 'script';

/** Version 1 Trusted Server APS renderer descriptor. */
export interface ApsRendererV1 {
  type: 'aps';
  version: 1;
  accountId: string;
  bidId: string;
  creativeId?: string | undefined;
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
  hb_pb?: string | undefined;
  hb_bidder?: string | undefined;
  hb_adid?: string | undefined;
  hb_cache_host?: string | undefined;
  hb_cache_path?: string | undefined;
  /** Trace-only OpenRTB bid identifier. */
  hb_bid_id?: string | undefined;
  /** Trace-only server-side auction identifier. */
  hb_auction_id?: string | undefined;
  /** Trace-only OpenRTB creative identifier. */
  hb_crid?: string | undefined;
  /** Trace hash of delivered creative markup. */
  hb_adm_hash?: string | undefined;
  nurl?: string | undefined;
  burl?: string | undefined;
  /** Typed winning-bid renderer capability. */
  renderer?: AuctionBidRenderer | undefined;
  /** Winning creative width used by the inline render bridge. */
  w?: number | undefined;
  /** Winning creative height used by the inline render bridge. */
  h?: number | undefined;
  /**
   * Sanitized winning creative markup for local rendering through the pbRender
   * bridge. Present whenever the winning bid carried a creative that passed the
   * server-side sanitize/rewrite boundary; absent when there was no creative or
   * it was rejected (e.g. over the 1 MiB cap), in which case the bridge falls
   * back to the PBS Cache coordinates. This is NOT gated by
   * `inject_adm_for_testing`.
   */
  adm?: string | undefined;
  /** Debug-only bid field mirror. Only present when `[debug] inject_adm_for_testing = true`. */
  debug_bid?: AuctionDebugBidData | undefined;
}

/** How a creative reached the page for a [`RenderRecord`]. */
export type RenderServedFrom = 'inline' | 'gam' | 'debug-adm' | 'pbs-cache' | 'prebid';

/** Client-side record joining a rendered creative to its auction winner. */
export interface RenderRecord {
  slotId: string;
  path: 'auction' | 'ssat' | 'gam-refresh';
  rendered: boolean;
  elementId?: string | undefined;
  auctionId?: string | undefined;
  bidder?: string | undefined;
  adId?: string | undefined;
  bidId?: string | undefined;
  creativeId?: string | undefined;
  admHash?: string | undefined;
  servedFrom?: RenderServedFrom | undefined;
  gamEmpty?: boolean | undefined;
  injected?: boolean | undefined;
  visible?: boolean | undefined;
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
  reason?: GptDiagnosticsBindingReason | undefined;
}

export interface GptDiagnosticsDurations {
  requestToResponseMs?: number | undefined;
  responseToRenderMs?: number | undefined;
  requestToRenderMs?: number | undefined;
  renderToLoadMs?: number | undefined;
  renderToViewableMs?: number | undefined;
}

export interface GptDiagnosticsRequestCycle {
  requestNumber: number;
  requestedAtMs?: number | undefined;
  responseAtMs?: number | undefined;
  renderAtMs?: number | undefined;
  loadAtMs?: number | undefined;
  viewableAtMs?: number | undefined;
  durations: GptDiagnosticsDurations;
  isEmpty?: boolean | undefined;
  size?: Size | undefined;
  isBackfill?: boolean | undefined;
  slotContentChanged?: boolean | undefined;
  incompleteSequence: boolean;
}

export interface GptDiagnosticsSlotExport {
  runtimeSlotNumber: number;
  slotElementId?: string | undefined;
  adUnitPath?: string | undefined;
  binding: GptDiagnosticsBinding;
  currentVisibilityPercentage?: number | undefined;
  maximumVisibilityPercentage?: number | undefined;
  requests: GptDiagnosticsRequestCycle[];
}

export interface GptDiagnosticsCallbackIssue {
  kind: GptDiagnosticsCallbackKind;
  runtimeSlotNumber: number;
  slotElementId?: string | undefined;
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
  setConfig?: ((cfg: Record<string, unknown>) => void) | undefined;
  getConfig?: (() => Record<string, unknown>) | undefined;
  requestAds?:
    | {
        (opts?: { bidsBackHandler?: (() => void) | undefined; timeout?: number | undefined }): void;
        (
          callback: () => void,
          opts?: {
            bidsBackHandler?: (() => void) | undefined;
            timeout?: number | undefined;
          }
        ): void;
      }
    | undefined;
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
  adSlots?: AuctionSlot[] | undefined;
  /** Winning bid targeting data injected before </body>. */
  bids?: Record<string, AuctionBidData> | undefined;
  /**
   * Bounded client-side Prebid APS renderer capabilities keyed by Prebid's generated
   * `hb_adid`. The Universal Creative bridge consumes each entry at most once.
   */
  apsPrebidRenderers?: Record<string, ApsPrebidRendererEntry> | undefined;
  /** Initialises GPT slots with server-side bid targeting and calls refresh(). */
  adInit?: (() => void) | undefined;
  /** Render-trace registry: latest render per slot. */
  renders?: Record<string, RenderRecord> | undefined;
  /** Append-only history of every render. */
  renderLog?: RenderRecord[] | undefined;
  /** Monotonic render generation for cancelling stale async work. */
  renderGeneration?: number | undefined;
  /** Page-global render sequence counter. */
  renderSeq?: number | undefined;
  /** GPT slot objects TS defined — used to destroy stale slots on SPA navigation. */
  prevGptSlots?: unknown[] | undefined;
  /** Guards one-time-per-page enableSingleRequest/enableServices calls. */
  servicesEnabled?: boolean | undefined;
  /** Maps actualDivId → slotId for slotRenderEnded billing lookup. */
  divToSlotId?: Record<string, string> | undefined;
  /**
   * Win/billing beacons already fired, keyed by `slotId|bidIdentity|kind|url`.
   * Used by the GPT render bridge so a bid's nurl/burl fire at most once even
   * across repeated Prebid Universal Creative requests for the same adId.
   */
  firedBeacons?: Record<string, boolean> | undefined;
  /** Slot-level GPT targeting keys TS applied on the previous route. */
  prevSlotTargetingKeys?: Record<string, string[]> | undefined;
  /**
   * One-shot bypass for the slim-Prebid refresh wrapper: true only while
   * adInit() runs its internal refresh of server-side-targeted slots, so the
   * wrapper passes that refresh straight to GPT instead of starting a
   * client-side auction that would clear the just-applied TS targeting.
   */
  adInitRefreshInProgress?: boolean | undefined;
  /**
   * Whether the publisher disabled GPT initial load through
   * `googletag.setConfig()` or `googletag.pubads().disableInitialLoad()`.
   * TS synchronizes this from GPT's getter and wraps both configuration APIs as
   * a fallback when the getter is unavailable.
   * When set, `display()` only registers a slot and the ad request must come
   * from a `refresh()`; adInit() uses this to refresh its own freshly defined
   * slots so they are not left blank.
   */
  gptInitialLoadDisabled?: boolean | undefined;
  /** Late publisher claims for TS-created GPT slots, keyed by actual div ID. */
  gptSlotHandoffs?: Record<string, GptSlotHandoff> | undefined;
  /** True only while TS calls a GPT function that the handoff wrappers observe. */
  gptSlotHandoffInternal?: boolean | undefined;
  /** Guards SPA pushState hook installation. */
  spaHookInstalled?: boolean | undefined;
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
  navGeneration?: number | undefined;
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
  scheduleInitialAdInit?:
    | ((initialBids?: Record<string, AuctionBidData> | undefined) => void)
    | undefined;
  /** Read-only GPT lifecycle diagnostics API, present only in an activated tab. */
  gptDiagnostics?: GptDiagnosticsApi | undefined;
}
