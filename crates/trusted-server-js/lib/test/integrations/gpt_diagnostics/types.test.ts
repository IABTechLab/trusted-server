import { describe, expect, expectTypeOf, it } from 'vitest';

import type {
  GptDiagnosticsApi,
  GptDiagnosticsAttributionIssue,
  GptDiagnosticsAttributionIssueReason,
  GptDiagnosticsExportV1,
  GptDiagnosticsRequestCycle,
  GptDiagnosticsSlotExport,
} from '../../../src/core/types';

describe('GPT diagnostics public types', () => {
  it('keeps the documented read-only API source-compatible', () => {
    const readOnlyApi: GptDiagnosticsApi = {
      snapshot: () => ({
        version: 1,
        capturedAt: '2026-08-04T00:00:00.000Z',
        page: { origin: 'https://example.com', pathname: '/' },
        slots: [],
        callbackIssues: [],
        coverage: {
          slotRequested: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotResponseReceived: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotRenderEnded: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotOnload: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          impressionViewable: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotVisibilityChanged: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
        },
        metadata: { droppedCallbacks: 0, evictedSlots: 0, evictedRequestCycles: 0 },
      }),
      export: () => undefined,
      subscribe: () => () => undefined,
      show: () => undefined,
      hide: () => undefined,
    };

    expectTypeOf(readOnlyApi).toEqualTypeOf<GptDiagnosticsApi>();
  });

  it('represents the versioned allowlist schema', () => {
    const slot: GptDiagnosticsSlotExport = {
      runtimeSlotNumber: 1,
      slotElementId: 'ad-slot-example',
      adUnitPath: '/example/site/banner',
      binding: { status: 'bound' },
      requests: [
        {
          requestNumber: 1,
          requestedAtMs: 10,
          responseAtMs: 20,
          durations: { requestToResponseMs: 10 },
          incompleteSequence: false,
        },
      ],
    };
    const snapshot: GptDiagnosticsExportV1 = {
      version: 1,
      capturedAt: '2026-07-28T00:00:00.000Z',
      page: {
        origin: 'https://example.com',
        pathname: '/article',
      },
      slots: [slot],
      callbackIssues: [],
      coverage: {
        slotRequested: { observed: 1, matched: 1, unmatched: 0, ambiguous: 0 },
        slotResponseReceived: { observed: 1, matched: 1, unmatched: 0, ambiguous: 0 },
        slotRenderEnded: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
        slotOnload: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
        impressionViewable: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
        slotVisibilityChanged: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
      },
      metadata: {
        droppedCallbacks: 0,
        evictedSlots: 0,
        evictedRequestCycles: 0,
      },
    };
    const evidenceCycle: GptDiagnosticsRequestCycle = {
      requestNumber: 2,
      durations: {},
      incompleteSequence: false,
      requestPath: 'competing',
      trustedServerOpportunity: 'renderable_candidate',
      trustedServerCreativeRequestAtMs: 20,
      trustedServerCreativeResponseAtMs: 25,
      trustedServerCreativeFailures: ['cache_fetch_failed'],
      delivery: 'trusted_server_response_sent',
    };
    const issue: GptDiagnosticsAttributionIssue = {
      reason: 'creative_attempt_expired',
      timestampMs: 30,
      runtimeSlotNumber: 1,
      slotElementId: 'ad-slot-example',
    };
    const issueReasons: GptDiagnosticsAttributionIssueReason[] = [
      'creative_request_without_slot',
      'creative_request_without_cycle',
      'creative_request_ambiguous_cycle',
      'creative_request_on_empty_cycle',
      'creative_attempt_capacity',
      'creative_attempt_unknown',
      'creative_attempt_expired',
      'creative_attempt_evicted',
    ];
    const evidenceSnapshot: GptDiagnosticsExportV1 = {
      ...snapshot,
      slots: [{ ...slot, requests: [evidenceCycle] }],
      attributionIssues: [issue],
      metadata: {
        ...snapshot.metadata,
        droppedAttributionIssues: 1,
      },
    };

    expect(snapshot.version).toBe(1);
    expect(evidenceSnapshot.slots[0]?.requests[0]).toBe(evidenceCycle);
    expect(evidenceSnapshot.attributionIssues?.[0]).toBe(issue);
    expect(issueReasons).toHaveLength(8);
    expect(JSON.stringify(snapshot)).not.toMatch(
      /bidder|targeting|creativeMarkup|auction|userId|cookie/i
    );
    expect(JSON.stringify(evidenceSnapshot)).not.toMatch(
      /price|targeting|bidId|markup|cacheUrl|payload|userId|cookie/i
    );
    expectTypeOf(snapshot).toEqualTypeOf<GptDiagnosticsExportV1>();
    expectTypeOf<GptDiagnosticsSlotExport>().not.toHaveProperty('bidder');
    expectTypeOf<GptDiagnosticsSlotExport>().not.toHaveProperty('targeting');
    expectTypeOf<GptDiagnosticsRequestCycle>().not.toHaveProperty('price');
    expectTypeOf<GptDiagnosticsRequestCycle>().not.toHaveProperty('targeting');
    expectTypeOf<GptDiagnosticsRequestCycle>().not.toHaveProperty('bidId');
    expectTypeOf<GptDiagnosticsRequestCycle>().not.toHaveProperty('markup');
    expectTypeOf<GptDiagnosticsRequestCycle>().not.toHaveProperty('cacheUrl');
    expectTypeOf<GptDiagnosticsRequestCycle>().not.toHaveProperty('payload');
    expectTypeOf<GptDiagnosticsAttributionIssue>().not.toHaveProperty('price');
    expectTypeOf<GptDiagnosticsAttributionIssue>().not.toHaveProperty('targeting');
    expectTypeOf<GptDiagnosticsAttributionIssue>().not.toHaveProperty('bidId');
    expectTypeOf<GptDiagnosticsAttributionIssue>().not.toHaveProperty('markup');
    expectTypeOf<GptDiagnosticsAttributionIssue>().not.toHaveProperty('cacheUrl');
    expectTypeOf<GptDiagnosticsAttributionIssue>().not.toHaveProperty('payload');
  });

  it('accepts publisher-refresh request-path evidence and request-intent correlation fields', () => {
    const cycle: GptDiagnosticsRequestCycle = {
      requestNumber: 3,
      durations: {},
      incompleteSequence: false,
      requestPath: 'publisher_refresh',
      requestIntentId: 7,
      trustedServerAuctionId: 'auction-a',
      opportunityToRequestMs: 24,
    };

    expect(cycle.requestPath).toBe('publisher_refresh');
    expect(cycle.requestIntentId).toBe(7);
    expect(cycle.trustedServerAuctionId).toBe('auction-a');
    expect(cycle.opportunityToRequestMs).toBe(24);
    expectTypeOf(cycle).toEqualTypeOf<GptDiagnosticsRequestCycle>();
  });

  it('keeps the Trusted Server auction ID and publisher-refresh recorder source-compatible', () => {
    const api: GptDiagnosticsApi = {
      snapshot: () => ({
        version: 1,
        capturedAt: '2026-08-05T00:00:00.000Z',
        page: { origin: 'https://example.com', pathname: '/' },
        slots: [],
        callbackIssues: [],
        coverage: {
          slotRequested: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotResponseReceived: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotRenderEnded: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotOnload: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          impressionViewable: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotVisibilityChanged: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
        },
        metadata: { droppedCallbacks: 0, evictedSlots: 0, evictedRequestCycles: 0 },
      }),
      export: () => undefined,
      subscribe: () => () => undefined,
      show: () => undefined,
      hide: () => undefined,
      recordTrustedServerOpportunity: (
        _slot,
        _auctionSlotId,
        _opportunity,
        _trustedServerAuctionId
      ) => undefined,
      recordPublisherRefresh: (_slots) => undefined,
    };

    expectTypeOf(api).toEqualTypeOf<GptDiagnosticsApi>();
  });
});
