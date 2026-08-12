// Shared TypeScript types for the tsjs core API and extensions.
export type Size = readonly [number, number];

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

export interface AdmRenderSourceV1 {
  type: 'adm';
  version: 1;
  adm: string;
  width: number;
  height: number;
}

export interface CacheRenderSourceV1 {
  type: 'cache';
  version: 1;
  cacheId: string;
  fetchUrl: string;
  width: number;
  height: number;
}

export interface CacheFetchPolicyV1 {
  version: 1;
  baseUrl: string;
}

export type BidRenderSourceV1 = ApsRendererV1 | AdmRenderSourceV1 | CacheRenderSourceV1;

export type AuctionSlotFailureReason =
  | 'auction_disabled'
  | 'consent_denied'
  | 'slot_not_eligible'
  | 'provider_timeout'
  | 'provider_error'
  | 'invalid_provider_response'
  | 'mediation_failed'
  | 'winner_not_renderable'
  | 'identity_generation_failed'
  | 'internal_error';

export type SlotAuctionDecisionV1 =
  | { slot: string; outcome: 'winner'; candidateId: string }
  | { slot: string; outcome: 'no_bid' }
  | { slot: string; outcome: 'failed'; reason: AuctionSlotFailureReason };

export interface AuctionDecisionSetV1 {
  version: 1;
  auctionId: string;
  results: SlotAuctionDecisionV1[];
}

export interface BrowserAuctionBidV1 {
  candidateId: string;
  slot: string;
  provider: string;
  upstreamBidId: string;
  cpm: number;
  currency: 'USD';
  targeting: Record<string, string>;
  rendererReservationId: string;
  renderSource: BidRenderSourceV1;
}

/** Exact GAM placement metadata required to publish one server-projected slot. */
export interface BrowserAuctionSlotV1 {
  slot: string;
  gamUnitPath: string;
  divId: string;
  formats: ReadonlyArray<Size>;
  targeting: Record<string, string>;
}

export interface BrowserAuctionProjectionV1 {
  version: 1;
  auction: AuctionDecisionSetV1;
  slots: BrowserAuctionSlotV1[];
  bids: BrowserAuctionBidV1[];
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

/** Release-internal critical module emitted inside the unified artifact. */
export interface BootManifestCriticalIntegrationV1 {
  readonly id: string;
  readonly phase: 'critical';
}

/** Release-internal later module authenticated and loaded by core. */
export interface BootManifestDeferredIntegrationV1 {
  readonly id: string;
  readonly phase: 'deferred';
  readonly trigger: 'first_display_or_idle';
  readonly src: string;
}

export type BootManifestIntegrationV1 =
  BootManifestCriticalIntegrationV1 | BootManifestDeferredIntegrationV1;

/** Exact phase-aware bundle set and injection order required by one TSJS release. */
export interface BootManifestV1 {
  readonly version: 1;
  readonly releaseId: string;
  readonly criticalSrc: string;
  readonly integrations: readonly BootManifestIntegrationV1[];
}

/** One direct-auction ad unit admitted into the current navigation. */
export interface ProgrammaticAdUnit {
  readonly code: string;
  readonly mediaTypes: Readonly<{
    banner: Readonly<{ sizes: readonly (readonly [number, number])[] }>;
  }>;
  readonly bids?: readonly Readonly<{
    bidder: string;
    params?: Readonly<Record<string, unknown>>;
  }>[];
}

export interface AddAdUnitsResult {
  readonly registered: readonly string[];
}

export interface RequestAdsOptions {
  readonly slots?: readonly string[];
  readonly timeoutMs?: number;
  readonly signal?: AbortSignal;
}

export type RenderFailureReason =
  | 'auction_timeout'
  | AuctionSlotFailureReason
  | 'network_error'
  | 'http_error'
  | 'invalid_response'
  | 'slot_unresolved'
  | 'descriptor_invalid'
  | 'invalid_dimensions'
  | 'dimensions_out_of_range'
  | 'no_render_source'
  | 'registry_full'
  | 'capability_registry_full'
  | 'external_queue_full'
  | 'external_ready_timeout'
  | 'external_artifact_incompatible'
  | 'prebid_admission_failed'
  | 'prebid_contract_violation'
  | 'prebid_selection_timeout'
  | 'reservation_collision'
  | 'identity_generation_failed'
  | 'cycle_unattributable'
  | 'slot_quarantined'
  | 'gpt_request_failed'
  | 'gpt_request_timeout'
  | 'gpt_completion_timeout'
  | 'reconciliation_capacity'
  | 'gam_empty'
  | 'bridge_claim_timeout'
  | 'bridge_id_mismatch'
  | 'owner_registration_timeout'
  | 'owner_insertion_timeout'
  | 'renderer_document_no_load'
  | 'runner_no_load'
  | 'runner_failed'
  | 'cache_network_error'
  | 'cache_http_error'
  | 'cache_invalid_response'
  | 'adm_document_no_load'
  | 'abi_mismatch'
  | 'bundle_partial';

export type RequestAdsSlotResult =
  | Readonly<{ slot: string; path: 'primary' | 'fallback'; outcome: 'accepted' }>
  | Readonly<{ slot: string; path: 'primary' | 'fallback'; outcome: 'no_bid' }>
  | Readonly<{
      slot: string;
      path: 'primary' | 'fallback';
      outcome: 'failed';
      reason: RenderFailureReason;
    }>
  | Readonly<{
      slot: string;
      path: 'primary' | 'fallback';
      outcome: 'cancelled';
      reason: 'caller_aborted' | 'superseded' | 'navigation_disposed';
    }>;

export interface RequestAdsResult {
  readonly slots: readonly RequestAdsSlotResult[];
}

export type TsjsLogLevel = 'silent' | 'error' | 'warn' | 'info' | 'debug';

export interface TsjsLog {
  setLevel(level: TsjsLogLevel): void;
  getLevel(): TsjsLogLevel;
  error(...values: readonly unknown[]): void;
  warn(...values: readonly unknown[]): void;
  info(...values: readonly unknown[]): void;
  debug(...values: readonly unknown[]): void;
}

export interface TsjsCommandQueue {
  readonly length: 0;
  push(callback: unknown): 0;
}

export interface CreativeBootV1 {
  readonly version: 1;
  readonly enabled: boolean;
  readonly clickGuard: boolean;
  readonly renderGuard: boolean;
}

export interface DiagnosticsBootV1 {
  readonly version: 1;
  readonly renderTraceOverlay: boolean;
  readonly gpt: Readonly<{ readonly active: boolean }>;
}

export interface TsjsBootV1 {
  readonly abi: 1;
  readonly releaseId: string;
  readonly manifest: Readonly<BootManifestV1>;
  readonly auctionProjection: Readonly<BrowserAuctionProjectionV1>;
  readonly cachePolicy?: Readonly<CacheFetchPolicyV1>;
  readonly creative: Readonly<CreativeBootV1>;
  readonly diagnostics: Readonly<DiagnosticsBootV1>;
}

export type RenderTracePathV1 = 'auction' | 'ssat' | 'gam-refresh';
export type RenderTraceServedFromV1 = 'inline' | 'gam' | 'debug-adm' | 'pbs-cache' | 'prebid';

export interface RenderTraceRecord {
  readonly slotId: string;
  readonly path: RenderTracePathV1;
  readonly rendered: boolean;
  readonly elementId?: string;
  readonly auctionId?: string;
  readonly bidder?: string;
  readonly adId?: string;
  readonly bidId?: string;
  readonly creativeId?: string;
  readonly admHash?: string;
  readonly servedFrom?: RenderTraceServedFromV1;
  readonly gamEmpty?: boolean;
  readonly injected?: boolean;
  readonly visible?: boolean;
  readonly count: number;
  readonly seq: number;
  readonly at: number;
}

export interface RenderTraceDiagnostics {
  current(): Readonly<Record<string, Readonly<RenderTraceRecord>>>;
  history(): readonly Readonly<RenderTraceRecord>[];
  subscribe(listener: (record: Readonly<RenderTraceRecord>) => void): () => void;
}

export interface TsjsDiagnostics {
  readonly renderTrace: RenderTraceDiagnostics;
  readonly gpt?: GptDiagnosticsApi;
}

export interface TsjsApiBase {
  readonly version: '1.0.0';
  readonly releaseId: string;
  readonly boot: Readonly<TsjsBootV1>;
  readonly que: TsjsCommandQueue;
  readonly log: TsjsLog;
  readonly _registerIntegration: (registration: unknown) => false;
  addAdUnits(units: ProgrammaticAdUnit | readonly ProgrammaticAdUnit[]): AddAdUnitsResult;
  requestAds(options?: RequestAdsOptions): Promise<RequestAdsResult>;
}

export interface TsjsKernelApi extends TsjsApiBase {
  readonly diagnostics: Readonly<TsjsDiagnostics>;
  readonly _internal: Readonly<{ state: 'kernel'; releaseId: string }>;
}

export interface TsjsFallbackApi extends TsjsApiBase {
  readonly diagnostics?: never;
  readonly _internal: Readonly<{
    state: 'fallback';
    releaseId: string;
    reason: 'abi_mismatch' | 'bundle_partial';
  }>;
}

export type TsjsApi = TsjsKernelApi | TsjsFallbackApi;
