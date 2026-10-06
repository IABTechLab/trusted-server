import { describe, expect, it } from 'vitest';

import type {
  GptDiagnosticsExportV1,
  GptDiagnosticsSlotExport,
  GptDiagnosticsRequestCycle,
  GptDiagnosticsBinding,
  GptDiagnosticsDurations,
  GptDiagnosticsCallbackIssue,
  GptDiagnosticsAttributionIssue,
  GptDiagnosticsCoverageCounters,
  GptDiagnosticsAdManagerIdentity,
} from '../../src/core/types';

type Treatment = 'copy' | 'exclude' | 'transform';

const root = {
  version: 'transform',
  capturedAt: 'copy',
  page: 'transform',
  slots: 'transform',
  callbackIssues: 'transform',
  attributionIssues: 'transform',
  coverage: 'transform',
  metadata: 'transform',
} satisfies Record<keyof GptDiagnosticsExportV1, Treatment>;
const page = { origin: 'copy', pathname: 'transform' } satisfies Record<
  keyof GptDiagnosticsExportV1['page'],
  Treatment
>;
const slot = {
  runtimeSlotNumber: 'copy',
  slotElementId: 'exclude',
  adUnitPath: 'exclude',
  binding: 'transform',
  currentVisibilityPercentage: 'copy',
  maximumVisibilityPercentage: 'copy',
  requests: 'transform',
} satisfies Record<keyof GptDiagnosticsSlotExport, Treatment>;
const cycle = {
  requestNumber: 'copy',
  requestedAtMs: 'copy',
  responseAtMs: 'copy',
  renderAtMs: 'copy',
  loadAtMs: 'copy',
  viewableAtMs: 'copy',
  durations: 'transform',
  isEmpty: 'copy',
  requestedSlotSizes: 'copy',
  size: 'copy',
  observedSlotSize: 'copy',
  isBackfill: 'copy',
  slotContentChanged: 'copy',
  incompleteSequence: 'copy',
  adManager: 'exclude',
  responseClass: 'copy',
  requestPath: 'copy',
  requestIntentId: 'copy',
  trustedServerAuctionId: 'copy',
  opportunityToRequestMs: 'copy',
  replacedRequestNumber: 'copy',
  previousRenderToRequestMs: 'copy',
  creativeChanged: 'copy',
  previousCreativeId: 'exclude',
  loadObservedBeforeRender: 'copy',
  trustedServerOpportunity: 'copy',
  trustedServerCreativeRequestAtMs: 'copy',
  trustedServerCreativeResponseAtMs: 'copy',
  trustedServerCreativeFailures: 'copy',
  delivery: 'copy',
} satisfies Record<keyof GptDiagnosticsRequestCycle, Treatment>;
const binding = { status: 'copy', reason: 'copy' } satisfies Record<
  keyof GptDiagnosticsBinding,
  Treatment
>;
const durations = {
  requestToResponseMs: 'copy',
  responseToRenderMs: 'copy',
  requestToRenderMs: 'copy',
  renderToLoadMs: 'copy',
  renderToViewableMs: 'copy',
} satisfies Record<keyof GptDiagnosticsDurations, Treatment>;
const callback = {
  kind: 'copy',
  runtimeSlotNumber: 'copy',
  slotElementId: 'exclude',
  timestampMs: 'copy',
  disposition: 'copy',
  reason: 'copy',
} satisfies Record<keyof GptDiagnosticsCallbackIssue, Treatment>;
const attribution = {
  reason: 'copy',
  timestampMs: 'copy',
  runtimeSlotNumber: 'copy',
  slotElementId: 'exclude',
} satisfies Record<keyof GptDiagnosticsAttributionIssue, Treatment>;
const counters = {
  observed: 'copy',
  matched: 'copy',
  unmatched: 'copy',
  ambiguous: 'copy',
} satisfies Record<keyof GptDiagnosticsCoverageCounters, Treatment>;
const coverage = {
  slotRequested: 'transform',
  slotResponseReceived: 'transform',
  slotRenderEnded: 'transform',
  slotOnload: 'transform',
  impressionViewable: 'transform',
  slotVisibilityChanged: 'transform',
} satisfies Record<keyof GptDiagnosticsExportV1['coverage'], Treatment>;
const metadata = {
  droppedCallbacks: 'copy',
  droppedAttributionIssues: 'copy',
  evictedSlots: 'copy',
  evictedRequestCycles: 'copy',
} satisfies Record<keyof GptDiagnosticsExportV1['metadata'], Treatment>;
const adManager = {
  lineItemId: 'exclude',
  creativeId: 'exclude',
  campaignId: 'exclude',
  advertiserId: 'exclude',
  sourceAgnosticLineItemId: 'exclude',
  sourceAgnosticCreativeId: 'exclude',
  yieldGroupIds: 'exclude',
  companyIds: 'exclude',
} satisfies Record<keyof GptDiagnosticsAdManagerIdentity, Treatment>;

// @ts-expect-error Future or missing source members must be classified explicitly.
const incomplete: Record<keyof GptDiagnosticsRequestCycle, Treatment> = { requestNumber: 'copy' };

describe('explicit current GPT source member classification', () => {
  it('records every excluded identifier and each deliberate transformation', () => {
    expect(cycle.adManager).toBe('exclude');
    expect(cycle.previousCreativeId).toBe('exclude');
    expect(slot.slotElementId).toBe('exclude');
    expect(slot.adUnitPath).toBe('exclude');
    expect(callback.slotElementId).toBe('exclude');
    expect(attribution.slotElementId).toBe('exclude');
    expect(Object.values(adManager).every((value) => value === 'exclude')).toBe(true);
    expect(root.version).toBe('transform');
    expect(page.pathname).toBe('transform');
    expect(Object.keys(binding)).toHaveLength(2);
    expect(Object.keys(durations)).toHaveLength(5);
    expect(Object.keys(counters)).toHaveLength(4);
    expect(Object.keys(coverage)).toHaveLength(6);
    expect(Object.keys(metadata)).toHaveLength(4);
    expect(incomplete.requestNumber).toBe('copy');
  });
});
