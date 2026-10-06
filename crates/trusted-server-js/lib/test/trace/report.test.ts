import { describe, expect, it } from 'vitest';

import { buildTraceReport } from '../../src/trace/report';

import {
  gptSourceFixture,
  reportFixture,
  TRACE_NOW,
  TRACE_ORIGIN,
  AUCTION_TOKEN,
  SLOT_TOKEN,
} from './fixtures';

function auction(number = 1) {
  return {
    schema_version: 1,
    diagnostic_auction_id: token(number),
    source: 'initial_navigation_ssat',
    terminal_status: 'completed',
    provider_calls: [],
    slots: [],
    truncation: { omitted_provider_calls: 0, omitted_slots: 0, omitted_nested_values: 0 },
    coverage: { provider_to_slot_no_bid: 'unavailable' },
  };
}
function token(number: number) {
  return `ts-auc-${number.toString(16).padStart(8, '0')}12344abc8def123456789abc`;
}
function sidecar(number = 1) {
  return {
    schema_version: 1,
    diagnostic_auction_id: token(number),
    slot_ref: SLOT_TOKEN,
    runtime_slot_number: 1,
    request_number: 1,
  };
}
function input() {
  return {
    requestContext: reportFixture().request_context,
    gptSource: gptSourceFixture(),
    origin: TRACE_ORIGIN,
    capturedAtMs: TRACE_NOW,
    collector: {
      serverAuctions: [],
      slotCorrelations: [],
      issues: [],
      omittedServerAuctions: 0,
      omittedSlotCorrelations: 0,
    },
  };
}
function success(result: ReturnType<typeof buildTraceReport>) {
  if (
    typeof result !== 'object' ||
    result === null ||
    !('ok' in result) ||
    result.ok !== true ||
    !('value' in result)
  )
    throw new Error('should build report');
  return result.value;
}
describe('combined trace capture', () => {
  it('rejects a caller capture-clock getter before reading it or invoking serialization', () => {
    let reads = 0;
    let serialized = 0;
    const source = input();
    Object.defineProperty(source, 'capturedAtMs', {
      enumerable: true,
      get() {
        reads += 1;
        return reads === 4
          ? {
              toJSON() {
                serialized += 1;
                return TRACE_NOW;
              },
            }
          : TRACE_NOW;
      },
    });
    expect(buildTraceReport(source)).toEqual({ ok: false, reason: 'invalid_snapshot' });
    expect(reads).toBe(0);
    expect(serialized).toBe(0);
  });
  it('uses a frozen top-level own data observation without caller get traps', () => {
    let reads = 0;
    const source = new Proxy(input(), {
      get() {
        reads += 1;
        throw new Error('private-get');
      },
    });
    const result = success(buildTraceReport(source));
    expect(result.stored_at_ms).toBe(TRACE_NOW);
    expect(reads).toBe(0);
  });
  it('projects an immutable report with independent capture time and original cookie observations', () => {
    const source = input();
    const result = success(buildTraceReport(source));
    expect(result.stored_at_ms).toBe(TRACE_NOW);
    expect(result.report.captured_at).toBe(new Date(TRACE_NOW).toISOString());
    expect(result.report.request_context).toEqual(source.requestContext);
    expect(result.report.gpt_diagnostics.page.pathname).toBe('/[redacted]');
    expect(JSON.stringify(result)).not.toContain('private-');
    expect(Object.isFrozen(result.report.gpt_diagnostics.slots[0].requests[0])).toBe(true);
    source.gptSource.slots[0].requests[0].requestNumber = 99;
    expect(result.report.gpt_diagnostics.slots[0].requests[0].requestNumber).toBe(1);
  });
  it('retains newest cardinality records and merges collector losses once', () => {
    const source = input();
    Object.assign(source.collector, {
      serverAuctions: Array.from({ length: 18 }, (_, i) => auction(i + 1)),
      slotCorrelations: Array.from({ length: 130 }, (_, i) => sidecar(i + 1)),
      omittedServerAuctions: 4,
      omittedSlotCorrelations: 7,
    });
    const result = success(buildTraceReport(source)).report;
    expect(result.server_auctions.map((record) => record.diagnostic_auction_id)).toEqual(
      Array.from({ length: 16 }, (_, i) => token(i + 3))
    );
    expect(result.slot_correlations).toHaveLength(128);
    expect(result.truncation.omitted_server_auctions).toBe(6);
    expect(result.truncation.omitted_slot_correlations).toBe(9);
    expect(result.truncation.omitted_request_cycles).toBe(0);
    expect(result.auction_coverage).toEqual({
      capture_status: 'partial',
      issues: ['record_evicted', 'correlation_unavailable'],
    });
  });
  it('rejects invalid inner evidence even if that record would be discarded', () => {
    const source = input();
    Object.assign(source.collector, {
      serverAuctions: [
        { ...auction(), private_value: 'secret' },
        ...Array.from({ length: 16 }, (_, i) => auction(i + 2)),
      ],
    });
    expect(buildTraceReport(source)).toEqual({ ok: false, reason: 'invalid_snapshot' });
  });
  it('rejects checked omission overflow', () => {
    const source = input();
    Object.assign(source.collector, {
      serverAuctions: Array.from({ length: 17 }, (_, i) => auction(i + 1)),
      omittedServerAuctions: 65535,
    });
    expect(buildTraceReport(source)).toEqual({ ok: false, reason: 'omission_counter_overflow' });
  });
  it('removes oldest non-floor cycles and their sidecars while preserving output order', () => {
    const source = input();
    const original = source.gptSource.slots[0].requests[0];
    source.gptSource.slots[0].requests = [
      { ...original, requestNumber: 1, requestedAtMs: 20 },
      { ...original, requestNumber: 2, requestedAtMs: undefined },
      { ...original, requestNumber: 3, requestedAtMs: 1 },
    ];
    delete source.gptSource.slots[0].requests[1].requestedAtMs;
    Object.assign(source.collector, {
      slotCorrelations: [{ ...sidecar(), diagnostic_auction_id: AUCTION_TOKEN, request_number: 2 }],
    });
    const full = success(buildTraceReport(source));
    const budget = new TextEncoder().encode(JSON.stringify(full)).length - 1;
    const result = success(buildTraceReport(source, budget)).report;
    expect(result.gpt_diagnostics.slots[0].requests.map((cycle) => cycle.requestNumber)).toEqual([
      1, 3,
    ]);
    expect(result.slot_correlations).toEqual([]);
    expect(result.truncation.omitted_request_cycles).toBe(1);
    expect(result.truncation.omitted_slot_correlations).toBe(1);
    expect(result.auction_coverage.capture_status).toBe('not_observed');
  });
  it('never removes a protected floor or substitutes an empty GPT snapshot', () => {
    expect(buildTraceReport(input(), 100)).toEqual({ ok: false, reason: 'snapshot_too_large' });
    expect(buildTraceReport(input(), 512 * 1024 + 1)).toEqual({
      ok: false,
      reason: 'invalid_snapshot',
    });
  });
  it('removes an uncorrelated auction and recomputes coverage when no evidence remains', () => {
    const source = input();
    source.gptSource.callbackIssues = [];
    source.gptSource.attributionIssues = [];
    Object.assign(source.collector, { serverAuctions: [auction()], slotCorrelations: [sidecar()] });
    const full = success(buildTraceReport(source));
    const result = success(
      buildTraceReport(source, new TextEncoder().encode(JSON.stringify(full)).length - 1)
    ).report;
    expect(result.server_auctions).toEqual([]);
    expect(result.slot_correlations).toEqual([]);
    expect(result.auction_coverage).toEqual({
      capture_status: 'unavailable',
      issues: ['record_evicted', 'correlation_unavailable'],
    });
  });
  it('protects any floor auction token even without a valid joining sidecar', () => {
    const source = input();
    source.gptSource.callbackIssues = [];
    source.gptSource.attributionIssues = [];
    Object.assign(source.collector, {
      serverAuctions: [{ ...auction(), diagnostic_auction_id: AUCTION_TOKEN }],
    });
    const full = success(buildTraceReport(source));
    expect(
      buildTraceReport(source, new TextEncoder().encode(JSON.stringify(full)).length - 1)
    ).toEqual({ ok: false, reason: 'snapshot_too_large' });
  });
  it('rejects a fractional capture clock and an API sidecar', () => {
    const source = input();
    expect(buildTraceReport({ ...source, capturedAtMs: TRACE_NOW + 0.5 })).toEqual({
      ok: false,
      reason: 'invalid_snapshot',
    });
    Object.assign(source.collector, {
      serverAuctions: [{ ...auction(), source: 'auction_api' }],
      slotCorrelations: [sidecar()],
    });
    expect(buildTraceReport(source)).toEqual({ ok: false, reason: 'invalid_snapshot' });
  });
  it('counts sidecars pruned when their known auction is discarded at initial cardinality', () => {
    const source = input();
    Object.assign(source.collector, {
      serverAuctions: Array.from({ length: 17 }, (_, i) => auction(i + 1)),
      slotCorrelations: [sidecar(1)],
    });
    const result = success(buildTraceReport(source)).report;
    expect(result.slot_correlations).toEqual([]);
    expect(result.truncation.omitted_slot_correlations).toBe(1);
  });
  it('rejects a capture clock outside the four-digit UTC range', () => {
    expect(buildTraceReport({ ...input(), capturedAtMs: Date.UTC(10000, 0, 1) })).toEqual({
      ok: false,
      reason: 'invalid_snapshot',
    });
  });
  it('removes oldest callback issues before attribution issues without reordering survivors', () => {
    const source = input();
    const issue = source.gptSource.callbackIssues[0];
    source.gptSource.callbackIssues = [
      { ...issue, timestampMs: 20 },
      { ...issue, timestampMs: 1 },
      { ...issue, timestampMs: 10 },
    ];
    const full = success(buildTraceReport(source));
    const result = success(
      buildTraceReport(source, new TextEncoder().encode(JSON.stringify(full)).length - 1)
    ).report;
    expect(result.gpt_diagnostics.callbackIssues.map((value) => value.timestampMs)).toEqual([
      20, 10,
    ]);
    expect(result.gpt_diagnostics.attributionIssues).toHaveLength(1);
    expect(result.truncation.omitted_callback_issues).toBe(1);
    expect(result.truncation.omitted_attribution_issues).toBe(0);
  });
  it('bounds a fully populated real-size fixture to 512 KiB while retaining every slot floor', () => {
    const source = input();
    source.requestContext.network = { region: 'é'.repeat(16), edge_region: '界'.repeat(42) };
    const slot = source.gptSource.slots[0];
    source.gptSource.slots = Array.from({ length: 64 }, (_, i) => ({
      ...structuredClone(slot),
      runtimeSlotNumber: i + 1,
      requests: Array.from({ length: 10 }, (_, j) => ({
        ...structuredClone(slot.requests[0]),
        requestNumber: j + 1,
        requestedAtMs: j + 1,
        requestedSlotSizes: Array.from({ length: 16 }, () => [100000, 100000] as [number, number]),
      })),
    }));
    source.gptSource.callbackIssues = Array.from({ length: 128 }, () =>
      structuredClone(source.gptSource.callbackIssues[0])
    );
    source.gptSource.attributionIssues = Array.from({ length: 128 }, () =>
      structuredClone(source.gptSource.attributionIssues![0])
    );
    Object.assign(source.collector, {
      serverAuctions: Array.from({ length: 16 }, (_, i) => ({
        ...auction(i + 1),
        provider_calls: Array.from({ length: 16 }, (_, j) => ({
          provider_number: j + 1,
          role: 'bidder',
          status: 'success',
          returned_bid_count: 65535,
          response_time_ms: 4294967295,
        })),
        slots: Array.from({ length: 64 }, (_, j) => ({
          slot_number: j + 1,
          slot_ref: SLOT_TOKEN,
          requested_sizes: Array.from({ length: 16 }, () => [100000, 100000]),
          returned_bid_count: 65535,
          candidate: 'selected',
          selected_creative_size: [100000, 100000],
        })),
      })),
      slotCorrelations: Array.from({ length: 128 }, (_, i) => ({
        ...sidecar(i + 1),
        request_number: 10,
        runtime_slot_number: (i % 64) + 1,
      })),
    });
    const result = success(buildTraceReport(source));
    expect(new TextEncoder().encode(JSON.stringify(result)).length).toBeLessThanOrEqual(512 * 1024);
    expect(result.report.gpt_diagnostics.slots).toHaveLength(64);
    expect(
      result.report.gpt_diagnostics.slots.every(
        (value) => value.requests[value.requests.length - 1]?.requestNumber === 10
      )
    ).toBe(true);
    expect(result.report.truncation.omitted_request_cycles).toBeGreaterThan(0);
    expect(result.report.truncation).toEqual({
      omitted_server_auctions: 3,
      omitted_slot_correlations: 3,
      omitted_request_cycles: 64 * 9,
      omitted_callback_issues: 128,
      omitted_attribution_issues: 128,
      omitted_nested_values: 0,
    });
    expect(result.report.server_auctions.map((record) => record.diagnostic_auction_id)).toEqual(
      Array.from({ length: 13 }, (_, i) => token(i + 4))
    );
    expect(result.report.slot_correlations.map((sidecar) => sidecar.diagnostic_auction_id)).toEqual(
      Array.from({ length: 125 }, (_, i) => token(i + 4))
    );
    expect(new TextEncoder().encode(JSON.stringify(result)).length).toBeGreaterThan(
      JSON.stringify(result).length
    );
    expect(result.report.request_context.cookies.diagnostics_session).toEqual({
      source: 'request',
      state: 'unavailable',
      detail: 'runtime_header_ambiguous',
    });
  }, 15000);
});
