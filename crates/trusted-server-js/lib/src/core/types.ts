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
export type AdTraceCoverageCategory =
  | 'gpt_requests'
  | 'gpt_responses'
  | 'gpt_renders'
  | 'gpt_loads'
  | 'gpt_viewability'
  | 'gpt_visibility'
  | 'prebid_render_succeeded'
  | 'prebid_render_failed';
export type AdTraceCorrelationResolution = 'correlated' | 'ambiguous' | 'unmatched' | 'ignored';
export interface AdTraceCoverageCounter {
  observed: number;
  correlated: number;
  ambiguous: number;
  unmatched: number;
  ignored: number;
}
export interface AdTraceCoverageObservation {
  category: AdTraceCoverageCategory;
  resolution: AdTraceCorrelationResolution;
  reason?: string;
}
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
  | 'gpt_impression_viewable'
  | 'gpt_slot_visibility_changed'
  | 'aps_display_bids_set'
  | 'aps_renderer_ready'
  | 'pb_render_requested'
  | 'pb_render_rejected'
  | 'pb_render_served'
  | 'direct_render_rejected'
  | 'creative_load_acknowledged'
  | 'creative_ack_timed_out'
  | 'creative_ack_superseded'
  | 'creative_ack_source_mismatched'
  | 'creative_ack_missing_token'
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
  responseClass?: AdTraceResponseClass;
  renderedWidth?: number;
  renderedHeight?: number;
  slotContentChanged?: boolean;
  sizeMatchesConfigured?: boolean;
  inViewPercentage?: number;
  prebidAuctionDurationMs?: number;
}

export interface AdTraceEvent extends AdTraceObservation {
  sequence: number;
  timestamp: number;
}

export type AdTraceResponseClass = 'empty' | 'backfill' | 'reservation' | 'unclassified_non_empty';
export type AdTraceAcknowledgementState =
  | 'confirmed'
  | 'timed_out'
  | 'superseded'
  | 'source_mismatched'
  | 'missing_token';
export type AdTraceGenerationTerminalState = 'active' | 'rendered' | 'empty' | 'superseded';
export interface AdTraceLifecycleDurations {
  requestToResponseMs?: number;
  responseToRenderMs?: number;
  renderToIframeLoadMs?: number;
  renderToCreativeAcknowledgementMs?: number;
  renderToViewableMs?: number;
  prebidAuctionMs?: number;
}
export interface GenerationTraceDiagnostics {
  requestNumber: number;
  terminalState: AdTraceGenerationTerminalState;
  responseClass?: AdTraceResponseClass;
  renderedSize?: readonly [number, number];
  slotContentChanged?: boolean;
  sizeMatchesConfigured?: boolean;
  currentVisibilityPercentage?: number;
  maximumVisibilityPercentage?: number;
  prebidRender?: 'succeeded' | 'failed';
  acknowledgement?: AdTraceAcknowledgementState;
  durations: AdTraceLifecycleDurations;
}
export interface GenerationTraceSnapshot {
  generation: number;
  stages: Record<AdTraceStageName, AdTraceStage>;
  diagnostics: GenerationTraceDiagnostics;
}

export interface SlotTraceSnapshot {
  slotId: string;
  latestGeneration: number;
  generations: GenerationTraceSnapshot[];
  /** Convenience view of only the latest retained generation. */
  stages: Record<AdTraceStageName, AdTraceStage>;
}

export type RenderTraceOutcome =
  | 'confirmed'
  | 'served'
  | 'timed_out'
  | 'gam_only'
  | 'empty'
  | 'unresolved';
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
  /** GPT reported this exact retained slot generation viewable. */
  viewability?: 'viewable';
  /** Bounded privacy-safe reason for the latest render evidence. */
  reason?: string;
  createdAt: number;
  updatedAt: number;
}

export interface AdTraceExport {
  version: 1;
  slots: SlotTraceSnapshot[];
  events: AdTraceEvent[];
  renders: RenderTraceSnapshot[];
  metadata: {
    droppedEvents: number;
    evictedSlots: number;
    coverage: Record<AdTraceCoverageCategory, AdTraceCoverageCounter>;
    anomalies: Record<string, number>;
  };
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
  /** Winning creative width; the bridge sizes the inline render from this. */
  w?: number;
  /** Winning creative height; the bridge sizes the inline render from this. */
  h?: number;
  nurl?: string;
  burl?: string;
  /** Typed winning-bid renderer capability. */
  renderer?: AuctionBidRenderer;
  /** Tester-gated trace; absent for ordinary traffic and malformed input. */
  trace?: TrustedServerBidTrace;
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

/**
 * Lifecycle state for a GPT slot TS created before its publisher declares it.
 *
 * Stored on `window.tsjs` so the head bootstrap and the full TSJS bundle share
 * one handoff protocol.
 */
export interface GptSlotHandoff {
  gamUnitPath: string;
  formats: Array<[number, number]>;
  publisherClaimed: boolean;
  suppressPublisherDisplay: boolean;
  suppressPublisherRefresh: boolean;
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
  /** Tester-gated terminal auction summary. */
  auctionTrace?: AuctionTraceSummary;
  /** Tester-only immutable diagnostic API. */
  adTrace?: AdTraceApi;
  /** Private recorder installed only by the optional ad_trace module. */
  recordAdTrace?: (observation: AdTraceObservation) => void;
  /** Private generation allocator installed only by the optional module. */
  nextAdTraceGeneration?: (slotId: string) => number;
  /** Record one privacy-safe callback-correlation result without raw identifiers. */
  recordAdTraceCoverage?: (observation: AdTraceCoverageObservation) => void;
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
    prebidAuctionDurationMs?: number;
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
    prebidAuctionDurationMs?: number;
  }>;
  /** Request-scoped root summaries retained until the GPT request boundary. */
  prebidServerSummaries?: Array<{
    auctionId: string;
    slotId: string;
    summary: AuctionTraceSummary;
  }>;
  /** Completed Prebid auctions used to identify request-scoped no-bid selections. */
  prebidCompletedAuctions?: Array<{
    auctionId: string;
    slotIds: string[];
    prebidAuctionDurationMs?: number;
  }>;
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
   * True once the publisher has disabled GPT initial load through
   * `googletag.setConfig()` or `googletag.pubads().disableInitialLoad()`.
   * GPT exposes no getter for this state, so TS tracks both configuration APIs.
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
}
