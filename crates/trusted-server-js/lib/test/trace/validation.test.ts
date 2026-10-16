import { describe, expect, it } from 'vitest';

import {
  parseTraceReport,
  parseTraceStoredReport,
  traceReportRejection,
  boundedJsonShape,
  validateTraceGptDiagnostics,
  validateTraceReport,
  validateTraceStoredReport,
} from '../../src/trace/report-validation';
import {
  TRACE_BINDING_REASONS,
  TRACE_CALLBACK_KINDS,
  TRACE_CALLBACK_REASONS,
  TRACE_ATTRIBUTION_REASONS,
  TRACE_RESPONSE_CLASSES,
  TRACE_REQUEST_PATHS,
  TRACE_OPPORTUNITIES,
  TRACE_DELIVERIES,
} from '../../src/trace/report-types';

import {
  projectedGptFixture,
  reportFixture,
  TRACE_ORIGIN,
  TRACE_TIME,
  TRACE_NOW,
} from './fixtures';
import { AUCTION_TOKEN, SLOT_TOKEN } from './fixtures';

function valid(value: unknown) {
  return validateTraceGptDiagnostics(value, TRACE_ORIGIN);
}

describe('strict combined report ingestion', () => {
  it('returns a frozen owned model instead of retaining a proxy that changes later reads', () => {
    const value = reportFixture();
    const original = value.gpt_diagnostics.metadata;
    let traps = 0;
    value.gpt_diagnostics.metadata = new Proxy(original, {
      get() {
        traps += 1;
        throw new Error('should not call');
      },
    });
    const model = parseTraceReport(value, TRACE_ORIGIN, TRACE_NOW);
    expect(model).toBeDefined();
    expect(traps).toBe(0);
    expect(Object.is(model, value)).toBe(false);
    expect(model?.gpt_diagnostics.metadata).not.toBe(original);
    expect(Object.isFrozen(original)).toBe(false);
    original.droppedCallbacks = 99;
    expect(model?.gpt_diagnostics.metadata.droppedCallbacks).toBe(0);
    const frozen = (object: unknown): void => {
      if (typeof object !== 'object' || object === null) return;
      expect(Object.isFrozen(object)).toBe(true);
      Object.values(object).forEach(frozen);
    };
    frozen(model);
    const wrapper = parseTraceStoredReport(
      { stored_at_ms: TRACE_NOW, report: reportFixture() },
      TRACE_ORIGIN,
      TRACE_NOW
    );
    expect(Object.isFrozen(wrapper)).toBe(true);
    expect(wrapper?.stored_at_ms).toBe(TRACE_NOW);
    expect(
      parseTraceStoredReport(
        { stored_at_ms: TRACE_NOW, report: reportFixture() },
        TRACE_ORIGIN,
        TRACE_NOW + 900001
      )
    ).toBeUndefined();
  });
  it('validates the same own counter and enum data that is measured for storage', () => {
    const counter = reportFixture();
    Object.assign(counter.gpt_diagnostics.metadata, { droppedCallbacks: 'private-counter' });
    counter.gpt_diagnostics.metadata = new Proxy(counter.gpt_diagnostics.metadata, {
      get(target, name, receiver) {
        return name === 'droppedCallbacks' ? 0 : Reflect.get(target, name, receiver);
      },
    });
    expect(validateTraceReport(counter, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
    const enumValue = projectedGptFixture();
    enumValue.slots[0].requests[0].delivery = 'private-delivery';
    enumValue.slots[0].requests[0] = new Proxy(enumValue.slots[0].requests[0], {
      get(target, name, receiver) {
        return name === 'delivery' ? 'unknown' : Reflect.get(target, name, receiver);
      },
    });
    expect(valid(enumValue)).toBe(false);
    const token = projectedGptFixture();
    token.slots[0].requests[0].trustedServerAuctionId = 'private-id';
    token.slots[0].requests[0] = new Proxy(token.slots[0].requests[0], {
      get(target, name, receiver) {
        return name === 'trustedServerAuctionId'
          ? AUCTION_TOKEN
          : Reflect.get(target, name, receiver);
      },
    });
    expect(valid(token)).toBe(false);
  });
  it('accepts numeric maxima and rejects fractional identifiers without changing fractional timing', () => {
    const value = projectedGptFixture();
    const cycle = value.slots[0].requests[0];
    Object.assign(cycle, {
      requestNumber: Number.MAX_SAFE_INTEGER,
      requestIntentId: 0,
      replacedRequestNumber: Number.MAX_SAFE_INTEGER,
      requestedAtMs: Number.MAX_SAFE_INTEGER,
      size: [100000, 100000],
      observedSlotSize: [0, 100000],
    });
    Object.assign(value.metadata, { droppedCallbacks: Number.MAX_SAFE_INTEGER });
    expect(valid(value)).toBe(true);
    cycle.requestNumber = 0.5;
    expect(valid(value)).toBe(false);
  });

  it('enforces the 255 UTF-8 byte origin cap at the exact boundary', () => {
    const prefix = `https://${'a'.repeat(63)}.${'a'.repeat(63)}.${'a'.repeat(63)}.`;
    const origin = prefix + 'b'.repeat(55);
    expect(new TextEncoder().encode(origin).length).toBe(255);
    const value = projectedGptFixture();
    value.page.origin = origin;
    expect(validateTraceGptDiagnostics(value, origin)).toBe(true);
    value.page.origin = origin + 'b';
    expect(validateTraceGptDiagnostics(value, origin + 'b')).toBe(false);
  });

  it('validates full nested report containers and the runtime ambiguity detail pair', () => {
    const value = reportFixture();
    expect(boundedJsonShape(value, 8)).toBeDefined();
    expect(boundedJsonShape(value, 7)).toBeUndefined();
    Object.assign(value.request_context.cookies.diagnostics_session, { state: 'present_invalid' });
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
  });
  it('returns bounded actionable categories for incompatible versions', () => {
    expect(traceReportRejection(reportFixture(), TRACE_ORIGIN, TRACE_NOW)).toBeUndefined();
    expect(
      traceReportRejection({ ...reportFixture(), schema_version: 2 }, TRACE_ORIGIN, TRACE_NOW)
    ).toBe('unsupported_report_version');
    const gpt = reportFixture();
    Object.assign(gpt.gpt_diagnostics, { schema_version: 2 });
    expect(traceReportRejection(gpt, TRACE_ORIGIN, TRACE_NOW)).toBe('unsupported_gpt_version');
    Object.assign(gpt.gpt_diagnostics, { schema_version: 1, source_schema_version: 2 });
    expect(traceReportRejection(gpt, TRACE_ORIGIN, TRACE_NOW)).toBe(
      'unsupported_gpt_source_version'
    );
    const auction = reportFixture();
    Object.assign(auction, { server_auctions: [{ schema_version: 2 }] });
    expect(traceReportRejection(auction, TRACE_ORIGIN, TRACE_NOW)).toBe(
      'unsupported_auction_version'
    );
    const correlation = reportFixture();
    Object.assign(correlation, { slot_correlations: [{ schema_version: 2 }] });
    expect(traceReportRejection(correlation, TRACE_ORIGIN, TRACE_NOW)).toBe(
      'unsupported_correlation_version'
    );
    expect(traceReportRejection({ private: 'private-error' }, TRACE_ORIGIN, TRACE_NOW)).toBe(
      'invalid_report'
    );
  });
  it('accepts every supported enum at its own boundary', () => {
    for (const reason of TRACE_BINDING_REASONS) {
      const value = projectedGptFixture();
      value.slots[0].binding.reason = reason;
      expect(valid(value)).toBe(true);
    }
    for (const kind of TRACE_CALLBACK_KINDS) {
      const value = projectedGptFixture();
      value.callbackIssues[0].kind = kind;
      expect(valid(value)).toBe(true);
    }
    for (const reason of TRACE_CALLBACK_REASONS) {
      const value = projectedGptFixture();
      value.callbackIssues[0].reason = reason;
      expect(valid(value)).toBe(true);
    }
    for (const reason of TRACE_ATTRIBUTION_REASONS) {
      const value = projectedGptFixture();
      value.attributionIssues[0].reason = reason;
      expect(valid(value)).toBe(true);
    }
    for (const [key, members] of [
      ['responseClass', TRACE_RESPONSE_CLASSES],
      ['requestPath', TRACE_REQUEST_PATHS],
      ['trustedServerOpportunity', TRACE_OPPORTUNITIES],
      ['delivery', TRACE_DELIVERIES],
    ] as const) {
      for (const member of members) {
        const value = projectedGptFixture();
        Object.assign(value.slots[0].requests[0], { [key]: member });
        expect(valid(value)).toBe(true);
      }
    }
  });

  it('counts actual nested containers from the report root and enforces compact UTF-8 bytes', () => {
    let depthTen: unknown = null;
    for (let index = 0; index < 10; index += 1) depthTen = { child: depthTen };
    expect(boundedJsonShape(depthTen, 10)).toBeDefined();
    expect(boundedJsonShape({ child: depthTen }, 10)).toBeUndefined();
    const text = { value: 'é😀' };
    const bytes = new TextEncoder().encode(JSON.stringify(text)).length;
    expect(boundedJsonShape(text, 10, bytes)).toBe(bytes);
    expect(boundedJsonShape(text, 10, bytes - 1)).toBeUndefined();
    const cyclical: { child?: unknown } = {};
    cyclical.child = cyclical;
    expect(boundedJsonShape(cyclical)).toBeUndefined();
    let calls = 0;
    expect(
      boundedJsonShape({
        toJSON() {
          calls += 1;
          return {};
        },
      })
    ).toBeUndefined();
    expect(calls).toBe(0);
  });

  it('rejects unsafe Unicode and controls at text-bearing boundaries', () => {
    for (const suffix of ['\u0000', '\u001f', '\u007f', '\u009f', '\u202e', '\u2066', '\ud800']) {
      const value = projectedGptFixture();
      value.page.origin = TRACE_ORIGIN + suffix;
      expect(valid(value)).toBe(false);
      value.page.origin = TRACE_ORIGIN;
      value.capturedAt = TRACE_TIME + suffix;
      expect(valid(value)).toBe(false);
    }
    const value = projectedGptFixture();
    value.page.pathname = '/private-path';
    expect(valid(value)).toBe(false);
  });

  it('enforces server/sidecar caps, status semantics and the API correlation exclusion', () => {
    const auction = {
      schema_version: 1,
      diagnostic_auction_id: AUCTION_TOKEN,
      source: 'initial_navigation_ssat',
      terminal_status: 'completed',
      provider_calls: [],
      slots: [],
      truncation: { omitted_provider_calls: 0, omitted_slots: 0, omitted_nested_values: 0 },
      coverage: { provider_to_slot_no_bid: 'unavailable' },
    };
    const correlation = {
      schema_version: 1,
      diagnostic_auction_id: AUCTION_TOKEN,
      slot_ref: SLOT_TOKEN,
      runtime_slot_number: 1,
      request_number: 1,
    };
    const value = reportFixture();
    Object.assign(value, {
      server_auctions: Array.from({ length: 16 }, () => auction),
      slot_correlations: Array.from({ length: 128 }, () => correlation),
    });
    value.auction_coverage.capture_status = 'complete';
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(true);
    Object.assign(value, { server_auctions: Array.from({ length: 17 }, () => auction) });
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
    Object.assign(value, {
      server_auctions: [auction],
      slot_correlations: Array.from({ length: 129 }, () => correlation),
    });
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
    Object.assign(value, { slot_correlations: [correlation] });
    Object.assign(value.auction_coverage, {
      capture_status: 'partial',
      issues: ['evidence_transport_failed'],
    });
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(true);
    auction.source = 'auction_api';
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
  });

  it('rejects a shape-valid combined report whose compact wrapper exceeds 512 KiB', () => {
    const gpt = projectedGptFixture();
    gpt.slots = Array.from({ length: 64 }, () => {
      const slot = projectedGptFixture().slots[0];
      slot.requests = Array.from({ length: 10 }, () => projectedGptFixture().slots[0].requests[0]);
      return slot;
    });
    expect(valid(gpt)).toBe(true);
    const report = reportFixture();
    report.gpt_diagnostics = gpt;
    const wrapper = { stored_at_ms: TRACE_NOW, report };
    expect(new TextEncoder().encode(JSON.stringify(wrapper)).length).toBeGreaterThan(512 * 1024);
    expect(validateTraceStoredReport(wrapper, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
  });
  it('accepts the explicit projection and complete wrapper, including zero CSS size and fractional browser durations', () => {
    expect(valid(projectedGptFixture())).toBe(true);
    expect(validateTraceReport(reportFixture(), TRACE_ORIGIN, TRACE_NOW)).toBe(true);
    expect(
      validateTraceStoredReport(
        { stored_at_ms: TRACE_NOW, report: reportFixture() },
        TRACE_ORIGIN,
        TRACE_NOW
      )
    ).toBe(true);
  });
  it('retains source optionality without requiring later optional fields', () => {
    const value = projectedGptFixture();
    Reflect.deleteProperty(value, 'attributionIssues');
    Reflect.deleteProperty(value.metadata, 'droppedAttributionIssues');
    value.slots = [
      {
        runtimeSlotNumber: 0,
        binding: { status: 'bound' },
        requests: [{ requestNumber: 0, durations: {}, incompleteSequence: true }],
      },
    ] as typeof value.slots;
    expect(valid(value)).toBe(true);
  });
  it.each([
    'adManager',
    'previousCreativeId',
    'slotElementId',
    'adUnitPath',
    '__proto__',
    'constructor',
    'future',
  ])('rejects forbidden request-cycle property %s', (key) => {
    const value = projectedGptFixture();
    Object.defineProperty(value.slots[0].requests[0], key, {
      enumerable: true,
      value: 'private-sentinel',
    });
    expect(valid(value)).toBe(false);
  });
  it('rejects extra keys at every nested boundary', () => {
    const value = projectedGptFixture();
    const objects = [
      value,
      value.page,
      value.slots[0],
      value.slots[0].binding,
      value.slots[0].requests[0],
      value.slots[0].requests[0].durations,
      value.callbackIssues[0],
      value.attributionIssues[0],
      value.coverage,
      value.coverage.slotRequested,
      value.metadata,
    ];
    for (const target of objects) {
      Object.assign(target, { future: 'private-sentinel' });
      expect(valid(value)).toBe(false);
      Reflect.deleteProperty(target, 'future');
    }
  });
  it.each([0, 2, '1', null])('rejects unsupported GPT/source/report versions %s', (version) => {
    expect(valid({ ...projectedGptFixture(), schema_version: version })).toBe(false);
    expect(valid({ ...projectedGptFixture(), source_schema_version: version })).toBe(false);
    expect(
      validateTraceReport({ ...reportFixture(), schema_version: version }, TRACE_ORIGIN, TRACE_NOW)
    ).toBe(false);
  });
  it.each([
    'https://attacker.example.com',
    'ftp://publisher.example.com',
    'https://publisher.example.com/',
    'https://user@publisher.example.com',
    'https://@publisher.example.com',
    'https://publisher.example.com?',
    'https://publisher.example.com#',
    'https://publisher.example.com\\',
    'https://publisher.example.com\n',
    'https://publisher.example.com,https://publisher.example.com',
  ])('rejects a foreign or repaired origin %s', (origin) => {
    const value = projectedGptFixture();
    value.page.origin = origin;
    expect(valid(value)).toBe(false);
  });
  it('accepts a canonical equivalent origin without changing the input', () => {
    const value = projectedGptFixture();
    value.page.origin = 'https://PUBLISHER.EXAMPLE.COM:443';
    expect(valid(value)).toBe(true);
    expect(value.page.origin).toBe('https://PUBLISHER.EXAMPLE.COM:443');
  });
  it.each(['adManager', 'previousCreativeId', 'slotElementId', 'adUnitPath'])(
    'rejects a prohibited slot/issue field %s',
    (key) => {
      for (const where of ['slot', 'callback', 'attribution']) {
        const value = projectedGptFixture();
        const target =
          where === 'slot'
            ? value.slots[0]
            : where === 'callback'
              ? value.callbackIssues[0]
              : value.attributionIssues[0];
        Object.assign(target, { [key]: 'private-sentinel' });
        expect(valid(value)).toBe(false);
      }
    }
  );
  it.each([NaN, Infinity, -1, Number.MAX_SAFE_INTEGER + 1, '1', null])(
    'rejects invalid browser timing %s',
    (time) => {
      const value = projectedGptFixture();
      Object.assign(value.slots[0].requests[0], { requestedAtMs: time });
      expect(valid(value)).toBe(false);
    }
  );
  it.each([-1, 100.1, NaN, Infinity, '1'])('rejects invalid visibility %s', (percentage) => {
    const value = projectedGptFixture();
    Object.assign(value.slots[0], { currentVisibilityPercentage: percentage });
    expect(valid(value)).toBe(false);
  });
  it('enforces each array and nested numeric bound at its edge', () => {
    const value = projectedGptFixture();
    value.slots = Array.from({ length: 64 }, () => projectedGptFixture().slots[0]);
    expect(valid(value)).toBe(true);
    value.slots.push(value.slots[0]);
    expect(valid(value)).toBe(false);
    const cycles = projectedGptFixture();
    cycles.slots[0].requests = Array.from(
      { length: 10 },
      () => projectedGptFixture().slots[0].requests[0]
    );
    expect(valid(cycles)).toBe(true);
    cycles.slots[0].requests.push(cycles.slots[0].requests[0]);
    expect(valid(cycles)).toBe(false);
    for (const key of ['callbackIssues', 'attributionIssues'] as const) {
      const issues = projectedGptFixture();
      Object.assign(issues, { [key]: Array.from({ length: 128 }, () => issues[key][0]) });
      expect(valid(issues)).toBe(true);
      issues[key].push(issues[key][0] as never);
      expect(valid(issues)).toBe(false);
    }
    const sizes = projectedGptFixture();
    sizes.slots[0].requests[0].requestedSlotSizes = Array.from({ length: 16 }, () => [1, 100000]);
    expect(valid(sizes)).toBe(true);
    sizes.slots[0].requests[0].requestedSlotSizes.push([1, 1]);
    expect(valid(sizes)).toBe(false);
    const failures = projectedGptFixture();
    failures.slots[0].requests[0].trustedServerCreativeFailures = Array.from(
      { length: 16 },
      () => 'missing_render_source'
    );
    expect(valid(failures)).toBe(true);
    failures.slots[0].requests[0].trustedServerCreativeFailures?.push('missing_render_source');
    expect(valid(failures)).toBe(false);
  });
  it('rejects invalid dimension shapes and retains zero-capable observed CSS boxes only', () => {
    for (const key of ['size', 'observedSlotSize'] as const) {
      for (const dimensions of [[-1, 1], [1.5, 1], [100001, 1], [1], [1, 1, 1], ['1', 1]]) {
        const value = projectedGptFixture();
        Object.assign(value.slots[0].requests[0], { [key]: dimensions });
        expect(valid(value)).toBe(false);
      }
    }
    const zero = projectedGptFixture();
    zero.slots[0].requests[0].size = [0, 0];
    expect(valid(zero)).toBe(false);
  });
  it('rejects unknown enums and malformed tokens without coercion', () => {
    const updates = [
      { responseClass: 'future' },
      { requestPath: '/private/path' },
      { trustedServerOpportunity: 'winner' },
      { delivery: 'rendered' },
      { trustedServerCreativeFailures: ['raw-error'] },
      { trustedServerAuctionId: 'internal-id' },
      { incompleteSequence: 1 },
    ];
    for (const update of updates) {
      const value = projectedGptFixture();
      Object.assign(value.slots[0].requests[0], update);
      expect(valid(value)).toBe(false);
    }
    for (const update of [{ status: 'future' }, { reason: 'private-error' }]) {
      const value = projectedGptFixture();
      Object.assign(value.slots[0].binding, update);
      expect(valid(value)).toBe(false);
    }
    for (const update of [
      { kind: 'future' },
      { disposition: 'future' },
      { reason: 'private-error' },
    ]) {
      const value = projectedGptFixture();
      Object.assign(value.callbackIssues[0], update);
      expect(valid(value)).toBe(false);
    }
    const attribution = projectedGptFixture();
    attribution.attributionIssues[0].reason = 'future';
    expect(valid(attribution)).toBe(false);
  });
  it('enforces finite integer counters and omission counters', () => {
    for (const count of [-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
      const value = projectedGptFixture();
      value.metadata.droppedCallbacks = count;
      expect(valid(value)).toBe(false);
    }
    for (const count of [65536, -1, 0.5]) {
      const value = reportFixture();
      value.truncation.omitted_nested_values = count;
      expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
    }
    const value = reportFixture();
    value.truncation.omitted_nested_values = 65535;
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(true);
  });
  it('requires capture times close to wrapper time and leaves earlier request time eligible', () => {
    const value = reportFixture();
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW + 60000)).toBe(true);
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW + 60001)).toBe(false);
    value.gpt_diagnostics.capturedAt = '2026-02-30T00:00:00Z';
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
    value.gpt_diagnostics.capturedAt = TRACE_TIME;
    value.request_context.captured_at = '2025-01-01T00:00:00Z';
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(true);
  });
  it('requires exact storage keys, finite times, bounded age and rollback rejection', () => {
    const wrapper = { stored_at_ms: TRACE_NOW, report: reportFixture() };
    expect(validateTraceStoredReport(wrapper, TRACE_ORIGIN, TRACE_NOW + 900000)).toBe(true);
    expect(validateTraceStoredReport(wrapper, TRACE_ORIGIN, TRACE_NOW + 900001)).toBe(false);
    expect(validateTraceStoredReport(wrapper, TRACE_ORIGIN, TRACE_NOW - 1)).toBe(true);
    expect(validateTraceStoredReport(wrapper, TRACE_ORIGIN, TRACE_NOW - 60000)).toBe(true);
    expect(validateTraceStoredReport(wrapper, TRACE_ORIGIN, TRACE_NOW - 60001)).toBe(false);
    expect(validateTraceStoredReport({ ...wrapper, future: 1 }, TRACE_ORIGIN, TRACE_NOW)).toBe(
      false
    );
    for (const stored_at_ms of [-1, NaN, Infinity, '1', null])
      expect(validateTraceStoredReport({ ...wrapper, stored_at_ms }, TRACE_ORIGIN, TRACE_NOW)).toBe(
        false
      );
  });
  it('recomputes capture status and requires distinct enum-order coverage issues', () => {
    const value = reportFixture();
    Object.assign(value.auction_coverage, {
      capture_status: 'unavailable',
      issues: ['record_evicted'],
    });
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(true);
    value.auction_coverage.capture_status = 'not_observed';
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
    Object.assign(value.auction_coverage, {
      capture_status: 'not_observed',
      issues: ['correlation_unavailable', 'external_client_side_unobservable'],
    });
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(true);
    Object.assign(value.auction_coverage, {
      issues: ['external_client_side_unobservable', 'correlation_unavailable'],
    });
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
    Object.assign(value.auction_coverage, {
      issues: ['record_evicted', 'record_evicted'],
      capture_status: 'unavailable',
    });
    expect(validateTraceReport(value, TRACE_ORIGIN, TRACE_NOW)).toBe(false);
  });
  it('rejects accessors and throwing proxies without invoking user serialization', () => {
    let calls = 0;
    const value = projectedGptFixture();
    Object.defineProperty(value.slots[0], 'binding', {
      enumerable: true,
      get() {
        calls += 1;
        throw new Error('should not call');
      },
    });
    expect(valid(value)).toBe(false);
    expect(calls).toBe(0);
    expect(
      valid(
        new Proxy(
          {},
          {
            getPrototypeOf() {
              throw new Error('should reject');
            },
          }
        )
      )
    ).toBe(false);
    expect(
      validateTraceStoredReport(
        new Proxy(
          {},
          {
            ownKeys() {
              throw new Error('should reject');
            },
          }
        ),
        TRACE_ORIGIN,
        TRACE_NOW
      )
    ).toBe(false);
  });
});
