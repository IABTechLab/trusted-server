import { describe, expect, expectTypeOf, it } from 'vitest';

import type {
  GptDiagnosticsApi,
  GptDiagnosticsAttributionIssue,
  GptDiagnosticsAttributionIssueReason,
  GptDiagnosticsDelivery,
  GptDiagnosticsExportV1,
  GptDiagnosticsRecorder,
  GptDiagnosticsRequestCycle,
  GptDiagnosticsRequestPath,
  GptDiagnosticsResponseClass,
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
        attributionIssues: [],
        coverage: {
          slotRequested: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotResponseReceived: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotRenderEnded: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotOnload: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          impressionViewable: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
          slotVisibilityChanged: { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 },
        },
        metadata: {
          droppedCallbacks: 0,
          droppedAttributionIssues: 0,
          evictedSlots: 0,
          evictedRequestCycles: 0,
        },
      }),
      export: () => undefined,
      subscribe: () => () => undefined,
      show: () => undefined,
      hide: () => undefined,
    };

    expectTypeOf(readOnlyApi).toEqualTypeOf<GptDiagnosticsApi>();
  });

  it('keeps evidence writers off the operator API and on the internal channel', () => {
    expectTypeOf<keyof GptDiagnosticsApi>().toEqualTypeOf<
      'snapshot' | 'export' | 'subscribe' | 'show' | 'hide'
    >();
    expectTypeOf<keyof GptDiagnosticsRecorder>().toEqualTypeOf<
      | 'recordTrustedServerOpportunity'
      | 'recordPrebidRefresh'
      | 'recordTrustedServerCreativeRequest'
      | 'recordTrustedServerCreativeResponse'
      | 'recordTrustedServerCreativeFailure'
    >();
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
      attributionIssues: [],
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
        droppedAttributionIssues: 0,
        evictedSlots: 0,
        evictedRequestCycles: 0,
      },
    };
    const evidenceCycle: GptDiagnosticsRequestCycle = {
      requestNumber: 2,
      durations: {},
      incompleteSequence: false,
      requestPath: 'publisher_refresh',
      requestIntentId: 7,
      trustedServerAuctionId: 'ts-auc-example',
      opportunityToRequestMs: 24,
      replacedRequestNumber: 1,
      previousRenderToRequestMs: 6048,
      previousCreativeId: 138563319574,
      creativeChanged: true,
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
    const evidenceSnapshot: GptDiagnosticsExportV1 = {
      ...snapshot,
      slots: [{ ...slot, requests: [evidenceCycle] }],
      attributionIssues: [issue],
      metadata: {
        ...snapshot.metadata,
        droppedAttributionIssues: 1,
      },
    };

    expect(JSON.stringify(snapshot)).not.toMatch(
      /bidder|targeting|creativeMarkup|auction|userId|cookie/i
    );
    expect(JSON.stringify(evidenceSnapshot)).not.toMatch(
      /price|targeting|bidId|markup|cacheUrl|payload|userId|cookie/i
    );
    expectTypeOf(evidenceCycle.requestPath).toEqualTypeOf<GptDiagnosticsRequestPath | undefined>();
    expectTypeOf(evidenceCycle.requestIntentId).toEqualTypeOf<number | undefined>();
    expectTypeOf(evidenceCycle.trustedServerAuctionId).toEqualTypeOf<string | undefined>();
    expectTypeOf(evidenceSnapshot.attributionIssues).toEqualTypeOf<
      readonly Readonly<GptDiagnosticsAttributionIssue>[]
    >();
    expectTypeOf(evidenceSnapshot.metadata.droppedAttributionIssues).toEqualTypeOf<number>();
    expectTypeOf<GptDiagnosticsAttributionIssueReason>().toEqualTypeOf<
      | 'creative_request_without_slot'
      | 'creative_request_without_cycle'
      | 'creative_request_ambiguous_cycle'
      | 'creative_request_on_empty_cycle'
      | 'creative_attempt_capacity'
      | 'creative_attempt_unknown'
      | 'creative_attempt_expired'
      | 'creative_attempt_evicted'
    >();
    // Keep additions synchronized with the exhaustive presentation switches.
    expectTypeOf<GptDiagnosticsDelivery>().toEqualTypeOf<
      | 'trusted_server_response_sent'
      | 'trusted_server_selected'
      | 'candidate_unconfirmed'
      | 'no_candidate'
      | 'unknown'
      | 'pending'
      | 'not_applicable'
    >();
    expectTypeOf<GptDiagnosticsRequestPath>().toEqualTypeOf<
      | 'trusted_server_direct'
      | 'prebid_refresh'
      | 'publisher_refresh'
      | 'competing'
      | 'unattributed'
    >();
    expectTypeOf<GptDiagnosticsResponseClass>().toEqualTypeOf<
      'empty' | 'backfill' | 'reservation' | 'unclassified_non_empty'
    >();
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
});
