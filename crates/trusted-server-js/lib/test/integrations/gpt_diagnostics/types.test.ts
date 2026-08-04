import { describe, expect, expectTypeOf, it } from 'vitest';

import type { GptDiagnosticsExportV1, GptDiagnosticsSlotExport } from '../../../src/core/types';

describe('GPT diagnostics public types', () => {
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

    expect(snapshot.version).toBe(1);
    expect(JSON.stringify(snapshot)).not.toMatch(
      /bidder|targeting|creativeMarkup|auction|userId|cookie/i
    );
    expectTypeOf(snapshot).toEqualTypeOf<GptDiagnosticsExportV1>();
    expectTypeOf<GptDiagnosticsSlotExport>().not.toHaveProperty('bidder');
    expectTypeOf<GptDiagnosticsSlotExport>().not.toHaveProperty('targeting');
  });
});
