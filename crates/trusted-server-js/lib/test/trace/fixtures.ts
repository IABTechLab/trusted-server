import type { GptDiagnosticsExportV1 } from '../../src/core/types';

export const TRACE_ORIGIN = 'https://publisher.example.com';
export const TRACE_TIME = '2026-10-05T10:15:30.123Z';
export const TRACE_NOW = Date.parse(TRACE_TIME);
export const AUCTION_TOKEN = 'ts-auc-1234567812344abc8def123456789abc';
export const SLOT_TOKEN = 'ts-slot-12345678-1234-4abc-8def-123456789abc';

/** Complete current TS Console fixture with separate forbidden identifier sentinels. */
export function gptSourceFixture(): GptDiagnosticsExportV1 {
  return {
    version: 1,
    capturedAt: TRACE_TIME,
    page: { origin: TRACE_ORIGIN, pathname: '/private-page-secret?private-query=secret' },
    slots: [
      {
        runtimeSlotNumber: 1,
        slotElementId: 'private-slot-element',
        adUnitPath: 'private-ad-unit',
        binding: { status: 'bound', reason: 'missing_element' },
        currentVisibilityPercentage: 12.5,
        maximumVisibilityPercentage: 100,
        requests: [
          {
            requestNumber: 1,
            requestedAtMs: 1.5,
            responseAtMs: 2,
            renderAtMs: 3,
            loadAtMs: 4,
            viewableAtMs: 5,
            durations: {
              requestToResponseMs: 0.5,
              responseToRenderMs: 1,
              requestToRenderMs: 1.5,
              renderToLoadMs: 1,
              renderToViewableMs: 2,
            },
            isEmpty: false,
            requestedSlotSizes: [
              [300, 250],
              [320, 50],
            ],
            size: [300, 250],
            observedSlotSize: [0, 0],
            isBackfill: true,
            slotContentChanged: true,
            incompleteSequence: false,
            adManager: {
              lineItemId: 900001,
              creativeId: 900002,
              campaignId: 900003,
              advertiserId: 900004,
              sourceAgnosticLineItemId: 900005,
              sourceAgnosticCreativeId: 900006,
              yieldGroupIds: [900007],
              companyIds: [900008],
            },
            responseClass: 'backfill',
            requestPath: 'trusted_server_direct',
            requestIntentId: 1,
            trustedServerAuctionId: AUCTION_TOKEN,
            opportunityToRequestMs: 0.5,
            replacedRequestNumber: 0,
            previousRenderToRequestMs: 0.5,
            creativeChanged: true,
            previousCreativeId: 900009,
            loadObservedBeforeRender: false,
            trustedServerOpportunity: 'renderable_candidate',
            trustedServerCreativeRequestAtMs: 1,
            trustedServerCreativeResponseAtMs: 2,
            trustedServerCreativeFailures: [
              'missing_render_source',
              'cache_fetch_failed',
              'invalid_cache_payload',
              'response_post_failed',
            ],
            delivery: 'trusted_server_response_sent',
          },
        ],
      },
    ],
    callbackIssues: [
      {
        kind: 'slotRenderEnded',
        runtimeSlotNumber: 1,
        slotElementId: 'private-callback-element',
        timestampMs: 0.5,
        disposition: 'matched',
        reason: 'invalid_event_order',
      },
    ],
    attributionIssues: [
      {
        reason: 'creative_request_without_slot',
        timestampMs: 0.5,
        runtimeSlotNumber: 1,
        slotElementId: 'private-attribution-element',
      },
    ],
    coverage: {
      slotRequested: { observed: 1, matched: 1, unmatched: 0, ambiguous: 0 },
      slotResponseReceived: { observed: 1, matched: 1, unmatched: 0, ambiguous: 0 },
      slotRenderEnded: { observed: 1, matched: 1, unmatched: 0, ambiguous: 0 },
      slotOnload: { observed: 1, matched: 1, unmatched: 0, ambiguous: 0 },
      impressionViewable: { observed: 1, matched: 1, unmatched: 0, ambiguous: 0 },
      slotVisibilityChanged: { observed: 1, matched: 1, unmatched: 0, ambiguous: 0 },
    },
    metadata: {
      droppedCallbacks: 0,
      droppedAttributionIssues: 0,
      evictedSlots: 0,
      evictedRequestCycles: 0,
    },
  };
}

/** Valid projected fixture built explicitly without any excluded source fields. */
export function projectedGptFixture() {
  const source = gptSourceFixture();
  const cycle = source.slots[0].requests[0];
  return {
    schema_version: 1,
    source_schema_version: 1,
    capturedAt: TRACE_TIME,
    page: { origin: TRACE_ORIGIN, pathname: '/[redacted]' },
    slots: [
      {
        runtimeSlotNumber: 1,
        binding: { status: 'bound', reason: 'missing_element' },
        currentVisibilityPercentage: 12.5,
        maximumVisibilityPercentage: 100,
        requests: [
          {
            requestNumber: 1,
            requestedAtMs: 1.5,
            responseAtMs: 2,
            renderAtMs: 3,
            loadAtMs: 4,
            viewableAtMs: 5,
            durations: cycle.durations,
            isEmpty: false,
            requestedSlotSizes: [
              [300, 250],
              [320, 50],
            ],
            size: [300, 250],
            observedSlotSize: [0, 0],
            isBackfill: true,
            slotContentChanged: true,
            incompleteSequence: false,
            responseClass: 'backfill',
            requestPath: 'trusted_server_direct',
            requestIntentId: 1,
            trustedServerAuctionId: AUCTION_TOKEN,
            opportunityToRequestMs: 0.5,
            replacedRequestNumber: 0,
            previousRenderToRequestMs: 0.5,
            creativeChanged: true,
            loadObservedBeforeRender: false,
            trustedServerOpportunity: 'renderable_candidate',
            trustedServerCreativeRequestAtMs: 1,
            trustedServerCreativeResponseAtMs: 2,
            trustedServerCreativeFailures: cycle.trustedServerCreativeFailures,
            delivery: 'trusted_server_response_sent',
          },
        ],
      },
    ],
    callbackIssues: [
      {
        kind: 'slotRenderEnded',
        runtimeSlotNumber: 1,
        timestampMs: 0.5,
        disposition: 'matched',
        reason: 'invalid_event_order',
      },
    ],
    attributionIssues: [
      { reason: 'creative_request_without_slot', timestampMs: 0.5, runtimeSlotNumber: 1 },
    ],
    coverage: source.coverage,
    metadata: source.metadata,
  };
}

export function reportFixture() {
  return {
    schema_version: 1,
    captured_at: TRACE_TIME,
    request_context: {
      schema_version: 1,
      captured_at: '2026-10-05T09:15:30Z',
      network: {},
      cookies: {
        ts_ec: { source: 'request', state: 'absent' },
        ts_eids: { source: 'request', state: 'absent' },
        ts_tester: { source: 'request', state: 'absent' },
        diagnostics_session: {
          source: 'request',
          state: 'unavailable',
          detail: 'runtime_header_ambiguous',
        },
      },
    },
    server_auctions: [],
    slot_correlations: [],
    gpt_diagnostics: projectedGptFixture(),
    auction_coverage: { capture_status: 'not_observed', issues: [] },
    truncation: {
      omitted_server_auctions: 0,
      omitted_slot_correlations: 0,
      omitted_request_cycles: 0,
      omitted_callback_issues: 0,
      omitted_attribution_issues: 0,
      omitted_nested_values: 0,
    },
  };
}
