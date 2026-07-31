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
  /** Opaque server-auction correlation ID used only by GPT diagnostics. */
  hb_auction_id?: string;
  /** Winning creative width; the bridge sizes the inline render from this. */
  w?: number;
  /** Winning creative height; the bridge sizes the inline render from this. */
  h?: number;
  nurl?: string;
  burl?: string;
  /** Typed winning-bid renderer capability. */
  renderer?: AuctionBidRenderer;
  /** Winning creative width used by the inline render bridge. */
  w?: number;
  /** Winning creative height used by the inline render bridge. */
  h?: number;
  /**
   * Sanitized winning creative markup for the inline render bridge. Present
   * when the server retained a non-empty creative; not gated by debug mode.
   */
  adm?: string;
  /** Debug-only bid field mirror. Only present when `[debug] inject_adm_for_testing = true`. */
  debug_bid?: AuctionDebugBidData;
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

/**
 * Ad Manager's own identifiers for the delivered ad, as reported by
 * `slotRenderEnded`.
 *
 * These are documented GPT callback fields carrying the publisher's own Ad
 * Manager data — the same values `?google_console=1` shows. They name what Ad
 * Manager delivered; they claim nothing about which demand source supplied it.
 */
export interface GptDiagnosticsAdManagerIdentity {
  lineItemId?: number;
  creativeId?: number;
  campaignId?: number;
  advertiserId?: number;
  sourceAgnosticLineItemId?: number;
  sourceAgnosticCreativeId?: number;
  yieldGroupIds?: number[];
  companyIds?: number[];
}

/**
 * How Ad Manager classified the delivered ad, derived only from the render
 * facts GPT reported.
 */
export type GptDiagnosticsResponseClass =
  | 'empty'
  | 'backfill'
  | 'reservation'
  | 'unclassified_non_empty';

/** The request path observed for a GPT request cycle. */
export type GptDiagnosticsRequestPath =
  | 'trusted_server_direct'
  | 'prebid_refresh'
  | 'publisher_refresh'
  | 'competing'
  | 'unattributed';

/** The Trusted Server creative opportunity observed for a request. */
export type GptDiagnosticsTrustedServerOpportunity =
  | 'renderable_candidate'
  | 'unrenderable_candidate'
  | 'no_candidate';

/** A safe failure category observed while obtaining or posting creative markup. */
export type GptDiagnosticsCreativeFailure =
  | 'missing_render_source'
  | 'cache_fetch_failed'
  | 'invalid_cache_payload'
  | 'response_post_failed';

/** Delivery evidence derived for a GPT request cycle. */
export type GptDiagnosticsDelivery =
  | 'trusted_server_response_sent'
  | 'trusted_server_selected'
  | 'candidate_unconfirmed'
  | 'no_candidate'
  | 'unknown'
  | 'pending'
  | 'not_applicable';

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
  adManager?: GptDiagnosticsAdManagerIdentity;
  responseClass?: GptDiagnosticsResponseClass;
  requestPath?: GptDiagnosticsRequestPath;
  requestIntentId?: number;
  trustedServerAuctionId?: string;
  opportunityToRequestMs?: number;
  replacedRequestNumber?: number;
  previousRenderToRequestMs?: number;
  creativeChanged?: boolean;
  previousCreativeId?: GptDiagnosticsAdManagerIdentity['creativeId'];
  loadObservedBeforeRender?: boolean;
  trustedServerOpportunity?: GptDiagnosticsTrustedServerOpportunity;
  trustedServerCreativeRequestAtMs?: number;
  trustedServerCreativeResponseAtMs?: number;
  trustedServerCreativeFailures?: GptDiagnosticsCreativeFailure[];
  /** Derived on every snapshot; absent only on a cycle read before derivation. */
  delivery?: GptDiagnosticsDelivery;
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

/** A safe reason that attribution evidence could not be associated or retained. */
export type GptDiagnosticsAttributionIssueReason =
  | 'creative_request_without_slot'
  | 'creative_request_without_cycle'
  | 'creative_request_ambiguous_cycle'
  | 'creative_request_on_empty_cycle'
  | 'creative_attempt_capacity'
  | 'creative_attempt_unknown'
  | 'creative_attempt_expired'
  | 'creative_attempt_evicted';

/** An attribution issue without auction-sensitive data. */
export interface GptDiagnosticsAttributionIssue {
  reason: GptDiagnosticsAttributionIssueReason;
  timestampMs: number;
  runtimeSlotNumber?: number;
  slotElementId?: string;
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
  attributionIssues?: GptDiagnosticsAttributionIssue[];
  coverage: Record<GptDiagnosticsCallbackKind, GptDiagnosticsCoverageCounters>;
  metadata: {
    droppedCallbacks: number;
    droppedAttributionIssues?: number;
    evictedSlots: number;
    evictedRequestCycles: number;
  };
}

/** GPT slot object identity, the only key diagnostics correlates slots by. */
export interface GptDiagnosticsSlotHandle {
  getSlotElementId?(): string;
  getAdUnitPath?(): string;
}

/** The documented, read-only operator API. It records no evidence. */
export interface GptDiagnosticsApi {
  snapshot(): GptDiagnosticsExportV1;
  export(): void;
  subscribe(listener: (snapshot: GptDiagnosticsExportV1) => void): () => void;
  show(): void;
  hide(): void;
}

/**
 * Evidence writers used by Trusted Server's own integration modules.
 *
 * Separate bundles can only reach each other through `window.tsjs`, so this
 * channel is reachable from the page like anything else there. Keeping it off
 * [`GptDiagnosticsApi`] is what makes the documented operator surface read-only
 * and stops the writers from becoming part of the public contract.
 */
export interface GptDiagnosticsRecorder {
  /** Record Trusted Server's creative opportunity for an associated GPT slot. */
  recordTrustedServerOpportunity(
    slot: GptDiagnosticsSlotHandle,
    auctionSlotId: string,
    opportunity: GptDiagnosticsTrustedServerOpportunity,
    trustedServerAuctionId?: string
  ): void;
  /** Mark slots whose next observed GPT request follows the Prebid refresh path. */
  recordPrebidRefresh(slots: GptDiagnosticsSlotHandle[]): void;
  /** Record a creative markup request and return its opaque attempt ID. */
  recordTrustedServerCreativeRequest(auctionSlotId: string): number | undefined;
  /** Record that a creative attempt successfully posted markup. */
  recordTrustedServerCreativeResponse(attemptId: number): void;
  /** Record a safe failure category for a creative attempt. */
  recordTrustedServerCreativeFailure(
    attemptId: number,
    reason: GptDiagnosticsCreativeFailure
  ): void;
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
  /** Scoped context marking an active Prebid-controlled GPT refresh delegation. */
  prebidRefreshDispatchInProgress?: boolean;
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
  /** Guards SPA pushState hook installation. */
  spaHookInstalled?: boolean;
  /**
   * Monotonic count of committed SPA navigations, incremented synchronously by
   * the SPA auction hook the moment it accepts a route change. The deferred
   * initial-adInit bootstrap ([`scheduleInitialAdInit`]) is pinned to
   * generation 0 (the SSR document) and no-ops when a navigation has
   * committed — before it was called, or while it was pending. A counter is
   * used instead of a URL comparison so the guard cannot diverge from the
   * auction path: a query-only history change (which the hook deliberately
   * ignores) leaves the counter unchanged, and an `/a → /b → /a` round trip
   * (where the URL compares equal again) advances it.
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
  /**
   * Internal evidence channel for Trusted Server integration modules. Not part
   * of the operator API; present only in an activated tab.
   */
  gptDiagnosticsRecorder?: GptDiagnosticsRecorder;
}
