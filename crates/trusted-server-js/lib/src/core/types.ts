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
  cache_id?: string | null;
  cache_host?: string | null;
  cache_path?: string | null;
  metadata?: Record<string, unknown>;
}

export type AuctionTraceSource = 'initial_navigation' | 'spa_navigation' | 'auction_api';
export type AuctionTraceOutcome = 'completed' | 'no_bid' | 'skipped' | 'failed' | 'abandoned';

/** Privacy-safe summary emitted only for configured tester traffic. */
export interface AuctionTraceSummary {
  version: 1;
  auctionTraceId: string;
  source: AuctionTraceSource;
  outcome: AuctionTraceOutcome;
}

/** Privacy-safe trace for one final Trusted Server winning bid. */
export interface TrustedServerBidTrace {
  version: 1;
  auctionTraceId: string;
  bidTraceId: string;
  source: AuctionTraceSource;
  slotId: string;
  provider: string;
  bidder: string;
}

export type AdTraceConfidence = 'definitive' | 'strong' | 'probable' | 'none';
export type AdTraceStageName = 'trustedServer' | 'prebid' | 'gam' | 'creative';
export interface AdTraceStage {
  outcome: string;
  confidence: AdTraceConfidence;
  reason: string;
}

export type AdTraceEventKind =
  | 'ts_auction_observed'
  | 'ts_winner_observed'
  | 'prebid_auction_init'
  | 'prebid_bid_response'
  | 'prebid_targeting_selected'
  | 'prebid_bid_won'
  | 'prebid_auction_end'
  | 'prebid_render_succeeded'
  | 'prebid_render_failed'
  | 'gpt_targeting_applied'
  | 'gpt_request_started'
  | 'gpt_slot_requested'
  | 'gpt_slot_response_received'
  | 'gpt_slot_render_ended'
  | 'gpt_slot_onload'
  | 'aps_display_bids_set'
  | 'pb_render_requested'
  | 'pb_render_rejected'
  | 'pb_render_served'
  | 'direct_render_rejected'
  | 'creative_load_acknowledged'
  | 'generation_superseded';

/** Sanitized observation accepted by the optional recorder. */
export interface AdTraceObservation {
  kind: AdTraceEventKind;
  slotId?: string;
  generation?: number;
  auctionTraceId?: string;
  bidTraceId?: string;
  provider?: string;
  bidder?: string;
  outcome?: string;
  confidence?: AdTraceConfidence;
  reason?: string;
  isEmpty?: boolean;
  isBackfill?: boolean;
}

export interface AdTraceEvent extends AdTraceObservation {
  sequence: number;
  timestamp: number;
}

export interface GenerationTraceSnapshot {
  generation: number;
  stages: Record<AdTraceStageName, AdTraceStage>;
}

export interface SlotTraceSnapshot {
  slotId: string;
  latestGeneration: number;
  generations: GenerationTraceSnapshot[];
  /** Convenience view of only the latest retained generation. */
  stages: Record<AdTraceStageName, AdTraceStage>;
}

export type RenderTraceOutcome = 'confirmed' | 'served' | 'gam_only' | 'empty' | 'unresolved';
export type RenderTraceVisibility = 'visible' | 'hidden' | 'disconnected' | 'unknown';

export interface RenderTraceSnapshot {
  sequence: number;
  slotId: string;
  generation: number;
  auctionTraceId?: string;
  bidTraceId?: string;
  source: 'gpt' | 'pb_render' | 'direct_auction';
  outcome: RenderTraceOutcome;
  confidence: AdTraceConfidence;
  visibility: RenderTraceVisibility;
  createdAt: number;
  updatedAt: number;
}

export interface AdTraceExport {
  version: 1;
  slots: SlotTraceSnapshot[];
  events: AdTraceEvent[];
  renders: RenderTraceSnapshot[];
  metadata: { droppedEvents: number; evictedSlots: number };
}

export interface AdTraceApi {
  getSlot(slotId: string): SlotTraceSnapshot | undefined;
  getEvents(): readonly AdTraceEvent[];
  getRenderTimeline(): readonly RenderTraceSnapshot[];
  export(): AdTraceExport;
}

/** Bid targeting data from the server-side auction, injected into `window.tsjs.bids`. */
export interface AuctionBidData {
  hb_pb?: string;
  hb_bidder?: string;
  hb_adid?: string;
  hb_cache_host?: string;
  hb_cache_path?: string;
  nurl?: string;
  burl?: string;
  /** Tester-gated trace; absent for ordinary traffic and malformed input. */
  trace?: TrustedServerBidTrace;
  /** Raw creative markup. Only present when `[debug] inject_adm_for_testing = true`. */
  adm?: string;
  /** Debug-only bid field mirror. Only present when `[debug] inject_adm_for_testing = true`. */
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
  /** Tester-gated terminal auction summary. */
  auctionTrace?: AuctionTraceSummary;
  /** Tester-only immutable diagnostic API. */
  adTrace?: AdTraceApi;
  /** Private recorder installed only by the optional ad_trace module. */
  recordAdTrace?: (observation: AdTraceObservation) => void;
  /** Private generation allocator installed only by the optional module. */
  nextAdTraceGeneration?: (slotId: string) => number;
  /** Private overlay subscription installed only by the optional module. */
  subscribeAdTrace?: (listener: () => void) => () => void;
  /** Bind one generation to the exact DOM element captured at its request boundary. */
  bindAdTraceElement?: (slotId: string, generation: number, element: HTMLElement) => void;
  /** Resolve only that exact captured element; never searches replacement DOM. */
  getAdTraceElement?: (slotId: string, generation: number) => HTMLElement | undefined;
  /** Private live visibility updater used only by the active overlay. */
  updateAdTraceVisibility?: (
    slotId: string,
    generation: number,
    visibility: RenderTraceVisibility
  ) => void;
  /** Private request-scoped Prebid correlation ledger; never exported. */
  prebidCorrelation?: Array<{
    auctionId: string;
    slotId: string;
    requestId: string;
    bidder?: string;
    adId?: string;
    traceToken?: string;
    serverTrace?: TrustedServerBidTrace;
    events?: AdTraceEventKind[];
  }>;
  /** Exact selected participants retained briefly for post-request terminal events. */
  prebidSelectedParticipants?: Array<{
    auctionId: string;
    slotId: string;
    requestId: string;
    adId?: string;
    traceToken?: string;
    bidder?: string;
    generation: number;
    selectedAt: number;
  }>;
  /** Request-scoped root summaries retained until the GPT request boundary. */
  prebidServerSummaries?: Array<{
    auctionId: string;
    slotId: string;
    summary: AuctionTraceSummary;
  }>;
  /** Completed Prebid auctions used to identify request-scoped no-bid selections. */
  prebidCompletedAuctions?: Array<{ auctionId: string; slotIds: string[] }>;
  /** Private bootstrap queue used until the GPT module installs its capture hook. */
  pendingAdTraceRequests?: Array<{
    slot: unknown;
    trigger: string;
    snapshot?: {
      slotId?: string;
      bidder?: string;
      adId?: string;
      traceToken?: string;
      bid?: AuctionBidData;
    };
  }>;
  /** Private request-boundary hook shared with bootstrap and slim Prebid. */
  captureAdTraceRequest?: (
    slot: unknown,
    trigger: string,
    snapshot?: {
      slotId?: string;
      bidder?: string;
      adId?: string;
      traceToken?: string;
      bid?: AuctionBidData;
    }
  ) => number;
  /** Initialises GPT slots with server-side bid targeting and calls refresh(). */
  adInit?: () => void;
  /** GPT slot objects TS defined — used to destroy stale slots on SPA navigation. */
  prevGptSlots?: unknown[];
  /** Guards one-time-per-page enableSingleRequest/enableServices calls. */
  servicesEnabled?: boolean;
  /** Maps actualDivId → slotId for slotRenderEnded billing lookup. */
  divToSlotId?: Record<string, string>;
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
