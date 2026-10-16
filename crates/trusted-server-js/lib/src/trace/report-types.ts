import type {
  TraceAuctionEvidenceV1,
  TraceRequestContextV1,
  TraceSlotCorrelationV1,
} from './types';

export const TRACE_CALLBACK_KINDS = [
  'slotRequested',
  'slotResponseReceived',
  'slotRenderEnded',
  'slotOnload',
  'impressionViewable',
  'slotVisibilityChanged',
] as const;
export const TRACE_BINDING_REASONS = [
  'missing_slot_element_id',
  'missing_element',
  'duplicate_dom_id',
  'dom_uniqueness_unverifiable',
  'duplicate_gpt_slot_id',
] as const;
export const TRACE_CALLBACK_REASONS = [
  'invalid_event_order',
  'missing_response_before_render',
  'invalid_visibility_percentage',
  'evicted_slot',
  'no_compatible_request_cycle',
  'overlapping_request_cycles',
] as const;
export const TRACE_ATTRIBUTION_REASONS = [
  'creative_request_without_slot',
  'creative_request_without_cycle',
  'creative_request_ambiguous_cycle',
  'creative_request_on_empty_cycle',
  'creative_attempt_capacity',
  'creative_attempt_unknown',
  'creative_attempt_expired',
  'creative_attempt_evicted',
] as const;
export const TRACE_RESPONSE_CLASSES = [
  'empty',
  'backfill',
  'reservation',
  'unclassified_non_empty',
] as const;
export const TRACE_REQUEST_PATHS = [
  'trusted_server_direct',
  'prebid_refresh',
  'publisher_refresh',
  'competing',
  'unattributed',
] as const;
export const TRACE_OPPORTUNITIES = [
  'renderable_candidate',
  'unrenderable_candidate',
  'no_candidate',
] as const;
export const TRACE_CREATIVE_FAILURES = [
  'missing_render_source',
  'cache_fetch_failed',
  'invalid_cache_payload',
  'response_post_failed',
] as const;
export const TRACE_DELIVERIES = [
  'trusted_server_response_sent',
  'trusted_server_selected',
  'candidate_unconfirmed',
  'no_candidate',
  'unknown',
  'pending',
  'not_applicable',
] as const;
export const TRACE_COVERAGE_ISSUES = [
  'evidence_projection_failed',
  'evidence_transport_failed',
  'evidence_validation_failed',
  'record_evicted',
  'correlation_unavailable',
  'external_client_side_unobservable',
] as const;
export type TraceCoverageIssue = (typeof TRACE_COVERAGE_ISSUES)[number];

/** Public request cycle facts, with delivery identifiers deliberately excluded. */
export interface TraceGptRequestCycle {
  readonly requestNumber: number;
  readonly requestedAtMs?: number;
  readonly responseAtMs?: number;
  readonly renderAtMs?: number;
  readonly loadAtMs?: number;
  readonly viewableAtMs?: number;
  readonly durations: {
    readonly requestToResponseMs?: number;
    readonly responseToRenderMs?: number;
    readonly requestToRenderMs?: number;
    readonly renderToLoadMs?: number;
    readonly renderToViewableMs?: number;
  };
  readonly isEmpty?: boolean;
  readonly requestedSlotSizes?: readonly (readonly [number, number])[];
  readonly size?: readonly [number, number];
  readonly observedSlotSize?: readonly [number, number];
  readonly isBackfill?: boolean;
  readonly slotContentChanged?: boolean;
  readonly incompleteSequence: boolean;
  readonly responseClass?: (typeof TRACE_RESPONSE_CLASSES)[number];
  readonly requestPath?: (typeof TRACE_REQUEST_PATHS)[number];
  readonly requestIntentId?: number;
  readonly trustedServerAuctionId?: string;
  readonly opportunityToRequestMs?: number;
  readonly replacedRequestNumber?: number;
  readonly previousRenderToRequestMs?: number;
  readonly creativeChanged?: boolean;
  readonly loadObservedBeforeRender?: boolean;
  readonly trustedServerOpportunity?: (typeof TRACE_OPPORTUNITIES)[number];
  readonly trustedServerCreativeRequestAtMs?: number;
  readonly trustedServerCreativeResponseAtMs?: number;
  readonly trustedServerCreativeFailures?: readonly (typeof TRACE_CREATIVE_FAILURES)[number][];
  readonly delivery?: (typeof TRACE_DELIVERIES)[number];
}

/** Trace-owned GPT snapshot, sourced from the explicitly supported TS Console version. */
export interface TraceGptDiagnosticsV1 {
  readonly schema_version: 1;
  readonly source_schema_version: 1;
  readonly capturedAt: string;
  readonly page: { readonly origin: string; readonly pathname: '/[redacted]' };
  readonly slots: readonly {
    readonly runtimeSlotNumber: number;
    readonly binding: {
      readonly status: 'bound' | 'unbound' | 'ambiguous';
      readonly reason?: (typeof TRACE_BINDING_REASONS)[number];
    };
    readonly currentVisibilityPercentage?: number;
    readonly maximumVisibilityPercentage?: number;
    readonly requests: readonly TraceGptRequestCycle[];
  }[];
  readonly callbackIssues: readonly {
    readonly kind: (typeof TRACE_CALLBACK_KINDS)[number];
    readonly runtimeSlotNumber: number;
    readonly timestampMs: number;
    readonly disposition: 'matched' | 'unmatched' | 'ambiguous';
    readonly reason: (typeof TRACE_CALLBACK_REASONS)[number];
  }[];
  readonly attributionIssues?: readonly {
    readonly reason: (typeof TRACE_ATTRIBUTION_REASONS)[number];
    readonly timestampMs: number;
    readonly runtimeSlotNumber?: number;
  }[];
  readonly coverage: Readonly<
    Record<
      (typeof TRACE_CALLBACK_KINDS)[number],
      Readonly<{ observed: number; matched: number; unmatched: number; ambiguous: number }>
    >
  >;
  readonly metadata: {
    readonly droppedCallbacks: number;
    readonly droppedAttributionIssues?: number;
    readonly evictedSlots: number;
    readonly evictedRequestCycles: number;
  };
}

/** Combined immutable public report used by the viewer and every export. */
export interface TraceReportV1 {
  readonly schema_version: 1;
  readonly captured_at: string;
  readonly request_context: TraceRequestContextV1;
  readonly server_auctions: readonly TraceAuctionEvidenceV1[];
  readonly slot_correlations: readonly TraceSlotCorrelationV1[];
  readonly gpt_diagnostics: TraceGptDiagnosticsV1;
  readonly auction_coverage: {
    readonly capture_status: 'complete' | 'partial' | 'unavailable' | 'not_observed';
    readonly issues: readonly TraceCoverageIssue[];
  };
  readonly truncation: {
    readonly omitted_server_auctions: number;
    readonly omitted_slot_correlations: number;
    readonly omitted_request_cycles: number;
    readonly omitted_callback_issues: number;
    readonly omitted_attribution_issues: number;
    readonly omitted_nested_values: number;
  };
}

/** Exact origin-local storage envelope; its timestamp is independent of request time. */
export interface TraceStoredReportV1 {
  readonly stored_at_ms: number;
  readonly report: TraceReportV1;
}
