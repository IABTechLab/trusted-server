/** A bounded shape fact about one owned runtime-visible cookie, never its value. */
export type CookieHealth = Readonly<
  | { source: 'request'; state: 'absent' }
  | {
      source: 'request';
      state: 'present_valid';
      detail:
        | 'valid_ec_format'
        | 'valid_eids_format'
        | 'valid_tester_value'
        | 'valid_diagnostics_value';
    }
  | {
      source: 'request';
      state: 'present_invalid';
      detail: 'malformed' | 'oversized' | 'unsupported_value';
    }
  | { source: 'request'; state: 'duplicate'; detail: 'multiple_values' }
  | {
      source: 'request';
      state: 'unavailable';
      detail: 'header_too_large' | 'header_not_utf8' | 'runtime_header_ambiguous';
    }
>;

/** The server-produced allowlist of redacted request facts. */
export interface TraceRequestContextV1 {
  readonly schema_version: 1;
  readonly captured_at: string;
  readonly network: {
    readonly masked_client_ip?: string;
    readonly country?: string;
    readonly region?: string;
    readonly asn?: number;
    readonly http_version?: string;
    readonly tls_protocol?: string;
    readonly tls_cipher?: string;
    readonly edge_hostname?: string;
    readonly edge_region?: string;
    readonly edge_pop?: string;
  };
  readonly cookies: {
    readonly ts_ec: CookieHealth;
    readonly ts_eids: CookieHealth;
    readonly ts_tester: CookieHealth;
    readonly diagnostics_session: CookieHealth;
  };
}

/** Exact server-assigned call sites, independent of browser delivery intent. */
export const TRACE_AUCTION_SOURCES = [
  'initial_navigation_ssat',
  'spa_page_bids',
  'auction_api',
] as const;
export const TRACE_TERMINAL_STATUSES = [
  'completed',
  'execution_failed',
  'dispatch_failed',
  'abandoned',
  'skipped',
] as const;
export const TRACE_TERMINAL_REASONS = [
  'policy_skipped',
  'no_eligible_slots',
  'no_provider_launched',
  'provider_execution_failed',
  'collection_failed',
  'unknown',
] as const;
export const TRACE_PROVIDER_ROLES = ['bidder', 'mediator', 'unknown'] as const;
export const TRACE_PROVIDER_STATUSES = [
  'success',
  'no_bid',
  'error',
  'pending',
  'abandoned',
  'unknown',
] as const;
export const TRACE_CANDIDATES = [
  'selected',
  'no_candidate',
  'selected_unrenderable',
  'unknown',
] as const;

/** Bounded auction-wide provider observations, without provider identity. */
export interface TraceProviderCall {
  readonly provider_number: number;
  readonly role: (typeof TRACE_PROVIDER_ROLES)[number];
  readonly status: (typeof TRACE_PROVIDER_STATUSES)[number];
  readonly response_time_ms?: number;
  readonly returned_bid_count: number;
}

/** Bounded slot candidate facts using only an opaque auction-local reference. */
export interface TraceAuctionSlot {
  readonly slot_number: number;
  readonly slot_ref: string;
  readonly requested_sizes: readonly (readonly [number, number])[];
  readonly returned_bid_count: number;
  readonly candidate: (typeof TRACE_CANDIDATES)[number];
  readonly selected_creative_size?: readonly [number, number];
}

/** Private SSAT/SPA identity carried into the existing GPT opportunity binding. */
export interface TraceGptIdentity {
  readonly diagnostic_auction_id: string;
  readonly slot_ref: string;
}

/** Server-produced public auction facts, distinct from auction/telemetry inputs. */
export interface TraceAuctionEvidenceV1 {
  readonly schema_version: 1;
  readonly diagnostic_auction_id: string;
  readonly source: (typeof TRACE_AUCTION_SOURCES)[number];
  readonly terminal_status: (typeof TRACE_TERMINAL_STATUSES)[number];
  readonly terminal_reason?: (typeof TRACE_TERMINAL_REASONS)[number];
  readonly total_time_ms?: number;
  readonly provider_calls: readonly TraceProviderCall[];
  readonly slots: readonly TraceAuctionSlot[];
  readonly truncation: {
    readonly omitted_provider_calls: number;
    readonly omitted_slots: number;
    readonly omitted_nested_values: number;
  };
  readonly coverage: { readonly provider_to_slot_no_bid: 'unavailable' };
}

/** Exactly one server evidence record or a bounded projection failure marker. */
export type TraceAuctionTransportV1 = Readonly<
  | {
      schema_version: 1;
      evidence: TraceAuctionEvidenceV1;
      unavailable_reason?: never;
    }
  | {
      schema_version: 1;
      unavailable_reason: 'evidence_projection_failed';
      evidence?: never;
    }
>;

/** Exact decision made by the existing GPT slot/request-cycle recorder. */
export interface TraceSlotCorrelationV1 {
  readonly schema_version: 1;
  readonly diagnostic_auction_id: string;
  readonly slot_ref: string;
  readonly runtime_slot_number: number;
  readonly request_number: number;
}
