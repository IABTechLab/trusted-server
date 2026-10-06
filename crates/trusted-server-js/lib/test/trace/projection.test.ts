import { describe, expect, it, vi } from 'vitest';

import { GptDiagnosticsStore } from '../../src/integrations/gpt_diagnostics/store';
import { addOmissions, projectTraceGptDiagnostics } from '../../src/trace/projection';

import { gptSourceFixture, projectedGptFixture, TRACE_ORIGIN } from './fixtures';
import { observedGptSource } from './gpt-fixtures';

describe('explicit GPT public projection', () => {
  const optionalPaths = [
    ['attributionIssues'],
    ['metadata', 'droppedAttributionIssues'],
    ['slots', '0', 'binding', 'reason'],
    ['slots', '0', 'currentVisibilityPercentage'],
    ['slots', '0', 'requests', '0', 'requestedAtMs'],
    ['slots', '0', 'requests', '0', 'durations', 'requestToResponseMs'],
    ['slots', '0', 'requests', '0', 'size'],
    ['slots', '0', 'requests', '0', 'observedSlotSize'],
    ['slots', '0', 'requests', '0', 'requestedSlotSizes'],
    ['slots', '0', 'requests', '0', 'trustedServerCreativeFailures'],
    ['slots', '0', 'requests', '0', 'trustedServerAuctionId'],
    ['attributionIssues', '0', 'runtimeSlotNumber'],
  ];
  function parentAt(source: unknown, path: string[]): Record<string, unknown> {
    let parent = source as Record<string, unknown>;
    for (const key of path.slice(0, -1)) parent = parent[key] as Record<string, unknown>;
    return parent;
  }
  it.each(optionalPaths)(
    'omits optional own undefined like an absent JSON member: %j',
    (...path) => {
      const absent = gptSourceFixture();
      const present = gptSourceFixture();
      const key = path[path.length - 1]!;
      delete parentAt(absent, path)[key];
      parentAt(present, path)[key] = undefined;
      const expected = projectTraceGptDiagnostics(absent, TRACE_ORIGIN);
      expect(expected.ok).toBe(true);
      expect(projectTraceGptDiagnostics(present, TRACE_ORIGIN)).toEqual(expected);
    }
  );
  it.each(['required', 'unknown', 'null', 'accessor', 'malformed'] as const)(
    'keeps %s public-source members strict while omitting optional undefined',
    (kind) => {
      const source = gptSourceFixture();
      const cycle = source.slots[0]!.requests[0]! as unknown as Record<string, unknown>;
      const getter = vi.fn(() => undefined);
      if (kind === 'required') cycle.requestNumber = undefined;
      if (kind === 'unknown') cycle.privateFutureField = undefined;
      if (kind === 'null') cycle.size = null;
      if (kind === 'accessor') Object.defineProperty(cycle, 'size', { get: getter });
      if (kind === 'malformed') cycle.size = ['300', 250];
      expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN)).toEqual({
        ok: false,
        reason: 'invalid_source',
      });
      expect(getter).not.toHaveBeenCalled();
    }
  );
  it.each(['no_candidate', 'renderable_candidate'] as const)(
    'projects actual pending %s API snapshots with optional undefined values',
    (opportunity) => {
      const store = new GptDiagnosticsStore({ now: () => 1 });
      const slot = { getSlotElementId: () => 'example' };
      store.recordTrustedServerOpportunity(slot, 'example', opportunity);
      store.recordSlotRequested(slot);
      const source = observedGptSource(store);
      expect(
        Object.getOwnPropertyDescriptor(source.slots[0]!.requests[0]!, 'size')?.value
      ).toBeUndefined();
      const projected = projectTraceGptDiagnostics(source, TRACE_ORIGIN);
      expect(projected.ok).toBe(true);
      if (!projected.ok) throw new Error('should project actual pending cycles');
      expect(projected.value.slots[0]!.requests[0]!.trustedServerOpportunity).toBe(opportunity);
      expect(
        Object.prototype.hasOwnProperty.call(projected.value.slots[0]!.requests[0]!, 'size')
      ).toBe(false);
    }
  );
  it('classifies inspection failures without reading caller-controlled errors', () => {
    let reads = 0;
    const failure = Object.defineProperty(new Error('private-message'), 'message', {
      get() {
        reads += 1;
        throw new Error('should not inspect');
      },
    });
    const source = new Proxy(gptSourceFixture(), {
      ownKeys() {
        throw failure;
      },
    });
    expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN)).toEqual({
      ok: false,
      reason: 'invalid_source',
    });
    expect(reads).toBe(0);
    const proxyError = new Proxy(new Error('private-message'), {
      getPrototypeOf() {
        throw new Error('should not inspect');
      },
    });
    expect(
      projectTraceGptDiagnostics(
        new Proxy(gptSourceFixture(), {
          ownKeys() {
            throw proxyError;
          },
        }),
        TRACE_ORIGIN
      )
    ).toEqual({ ok: false, reason: 'invalid_source' });
  });
  it('copies every public source member and excludes every private sentinel', () => {
    const source = gptSourceFixture();
    const result = projectTraceGptDiagnostics(source, TRACE_ORIGIN);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error('should project source');
    expect(result.value).toEqual(projectedGptFixture());
    expect(result.omittedNestedValues).toBe(0);
    const json = JSON.stringify(result.value);
    for (const sentinel of [
      'private-page-secret',
      'private-slot-element',
      'private-ad-unit',
      'private-callback-element',
      'private-attribution-element',
      '900001',
      '900002',
      '900003',
      '900004',
      '900005',
      '900006',
      '900007',
      '900008',
      '900009',
      'adManager',
      'previousCreativeId',
      'slotElementId',
      'adUnitPath',
    ])
      expect(json).not.toContain(sentinel);
    source.slots[0].requests[0].durations.requestToResponseMs = 999;
    source.slots[0].requests[0].requestedSlotSizes = [[1, 1]];
    expect(result.value.slots[0].requests[0].durations.requestToResponseMs).toBe(0.5);
    expect(result.value.slots[0].requests[0].requestedSlotSizes).toEqual([
      [300, 250],
      [320, 50],
    ]);
    const frozen = (value: unknown): void => {
      if (typeof value !== 'object' || value === null) return;
      expect(Object.isFrozen(value)).toBe(true);
      Object.values(value).forEach(frozen);
    };
    frozen(result.value);
    expect(Object.isFrozen(source.slots[0].requests[0])).toBe(false);
  });
  it('does not inspect excluded contents and rejects source accessors without invoking them', () => {
    const source = gptSourceFixture();
    let calls = 0;
    Object.assign(source.slots[0].requests[0], {
      adManager: new Proxy(
        {},
        {
          get() {
            calls += 1;
            throw new Error('should not call');
          },
          getPrototypeOf() {
            calls += 1;
            throw new Error('should not call');
          },
        }
      ),
      previousCreativeId: { private: 'ignored' },
    });
    Object.assign(source.slots[0], { slotElementId: '\ud800', adUnitPath: 42 });
    source.page.pathname = '\u0000' + 'x'.repeat(1024);
    expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN).ok).toBe(true);
    expect(calls).toBe(0);
    Object.defineProperty(source.slots[0].requests[0], 'adManager', {
      enumerable: true,
      get() {
        calls += 1;
        return {};
      },
    });
    expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN)).toEqual({
      ok: false,
      reason: 'invalid_source',
    });
    expect(calls).toBe(0);
  });
  it('rejects every extra source key at each source boundary', () => {
    const source = gptSourceFixture();
    const targets = [
      source,
      source.page,
      source.slots[0],
      source.slots[0].binding,
      source.slots[0].requests[0],
      source.slots[0].requests[0].durations,
      source.callbackIssues[0],
      source.attributionIssues![0],
      source.coverage,
      source.coverage.slotRequested,
      source.metadata,
    ];
    for (const target of targets) {
      Object.assign(target, { future: 'private-value' });
      expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN)).toEqual({
        ok: false,
        reason: 'invalid_source',
      });
      Reflect.deleteProperty(target, 'future');
    }
  });
  it('rejects invalid retained values and incompatible source versions without repairs', () => {
    const source = gptSourceFixture();
    Object.assign(source, { version: 2 });
    expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN)).toEqual({
      ok: false,
      reason: 'unsupported_source_version',
    });
    Object.assign(source, { version: 1 });
    Object.assign(source.slots[0].requests[0], { requestedAtMs: '1' });
    expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN)).toEqual({
      ok: false,
      reason: 'invalid_source',
    });
    source.slots[0].requests[0].requestedAtMs = 0.5;
    source.page.origin = 'https://attacker.example.com';
    expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN).ok).toBe(false);
  });
  it('preserves source optionality and canonicalizes origin during literal path redaction', () => {
    const source = gptSourceFixture();
    delete source.attributionIssues;
    delete source.metadata.droppedAttributionIssues;
    source.page.origin = 'https://PUBLISHER.EXAMPLE.COM:443';
    const result = projectTraceGptDiagnostics(source, TRACE_ORIGIN);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error('should project source');
    expect(result.value.page).toEqual({ origin: TRACE_ORIGIN, pathname: '/[redacted]' });
    expect(result.value).not.toHaveProperty('attributionIssues');
    expect(result.value.metadata).not.toHaveProperty('droppedAttributionIssues');
  });
  it('retains first sixteen nested values, validates omitted values and counts each omission once', () => {
    const source = gptSourceFixture();
    source.slots[0].requests[0].requestedSlotSizes = Array.from({ length: 17 }, (_, index) => [
      index + 1,
      1,
    ]);
    source.slots[0].requests[0].trustedServerCreativeFailures = Array.from(
      { length: 17 },
      () => 'missing_render_source'
    );
    const result = projectTraceGptDiagnostics(source, TRACE_ORIGIN);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error('should project source');
    expect(result.omittedNestedValues).toBe(2);
    expect(result.value.slots[0].requests[0].requestedSlotSizes).toHaveLength(16);
    expect(result.value.slots[0].requests[0].requestedSlotSizes?.[15]).toEqual([16, 1]);
    Object.assign(source.slots[0].requests[0], {
      trustedServerCreativeFailures: [
        ...source.slots[0].requests[0].trustedServerCreativeFailures!,
        'private-error',
      ],
    });
    expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN)).toEqual({
      ok: false,
      reason: 'invalid_source',
    });
  });
  it('rejects unsupported source cardinality and omission overflow', () => {
    const source = gptSourceFixture();
    source.slots = Array.from({ length: 65 }, () => gptSourceFixture().slots[0]);
    expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN)).toEqual({
      ok: false,
      reason: 'invalid_source',
    });
    const cycles = gptSourceFixture();
    cycles.slots[0].requests = Array.from(
      { length: 11 },
      () => gptSourceFixture().slots[0].requests[0]
    );
    expect(projectTraceGptDiagnostics(cycles, TRACE_ORIGIN).ok).toBe(false);
    const oversized = gptSourceFixture();
    oversized.slots[0].requests[0].trustedServerCreativeFailures = Array.from(
      { length: 65552 },
      () => 'missing_render_source'
    );
    expect(projectTraceGptDiagnostics(oversized, TRACE_ORIGIN)).toEqual({
      ok: false,
      reason: 'omission_counter_overflow',
    });
  });
  it('never invokes source get traps, array methods or iterators', () => {
    const source = gptSourceFixture();
    let calls = 0;
    source.slots[0].requests[0] = new Proxy(source.slots[0].requests[0], {
      get() {
        calls += 1;
        throw new Error('should not call');
      },
    });
    source.slots = new Proxy(source.slots, {
      get() {
        calls += 1;
        throw new Error('should not call');
      },
    });
    expect(projectTraceGptDiagnostics(source, TRACE_ORIGIN).ok).toBe(true);
    expect(calls).toBe(0);
  });
});
describe('checked public omission counters', () => {
  it('adds exact zero and u16-boundary counts', () => {
    expect(addOmissions(0, 0)).toBe(0);
    expect(addOmissions(65534, 1)).toBe(65535);
    expect(addOmissions(65535, 0)).toBe(65535);
  });
  it.each([
    [65535, 1],
    [-1, 1],
    [0, -1],
    [0, 0.5],
    [NaN, 0],
    [0, Infinity],
    [65536, 0],
  ])('rejects invalid or overflowing %s + %s', (current, added) => {
    expect(() => addOmissions(current, added)).toThrow('omission_counter_overflow');
  });
});
