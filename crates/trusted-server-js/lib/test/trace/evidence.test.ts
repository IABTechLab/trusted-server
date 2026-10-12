import { describe, expect, it } from 'vitest';

import type { TraceAuctionTransportV1 } from '../../src/trace/types';
import {
  validateTraceAuctionEvidence,
  validateTraceAuctionTransport,
  validateTraceSlotCorrelation,
  parseTraceAuctionTransport,
  parseTraceSlotCorrelation,
} from '../../src/trace/validation';

const AUCTION = 'ts-auc-1234567812344abc8def123456789abc';
const SLOT = 'ts-slot-12345678-1234-4abc-8def-123456789abc';

describe('immutable evidence ingestion', () => {
  it('rejects invalid own fields masked by caller get traps', () => {
    const original = evidence();
    Object.assign(original, { terminal_status: 'private-invalid-enum' });
    let reads = 0;
    const wrapped = new Proxy(original, {
      get(target, key, receiver) {
        reads += 1;
        if (key === 'terminal_status') return 'completed';
        return Reflect.get(target, key, receiver);
      },
    });
    expect(parseTraceAuctionTransport({ schema_version: 1, evidence: wrapped })).toBeUndefined();
    expect(reads).toBe(0);
    const invalidSidecar = {
      schema_version: 1,
      diagnostic_auction_id: 'private-invalid-token',
      slot_ref: SLOT,
      runtime_slot_number: 1,
      request_number: 1,
    };
    expect(
      parseTraceSlotCorrelation(
        new Proxy(invalidSidecar, {
          get(target, key, receiver) {
            reads += 1;
            if (key === 'diagnostic_auction_id') return AUCTION;
            return Reflect.get(target, key, receiver);
          },
        })
      )
    ).toBeUndefined();
    expect(reads).toBe(0);
  });
  it('ingests recursively wrapped own data without reading caller properties', () => {
    let reads = 0;
    const wrap = (value: unknown): unknown => {
      if (value === null || typeof value !== 'object') return value;
      const data = Array.isArray(value)
        ? value.map(wrap)
        : Object.fromEntries(Object.entries(value).map(([key, item]) => [key, wrap(item)]));
      return new Proxy(data, {
        get() {
          reads += 1;
          throw new Error('private-get');
        },
      });
    };
    const value = parseTraceAuctionTransport(wrap({ schema_version: 1, evidence: evidence() }));
    expect(value).toEqual({ schema_version: 1, evidence: evidence() });
    expect(
      parseTraceSlotCorrelation(
        wrap({
          schema_version: 1,
          diagnostic_auction_id: AUCTION,
          slot_ref: SLOT,
          runtime_slot_number: 1,
          request_number: 1,
        })
      )
    ).toBeDefined();
    expect(reads).toBe(0);
  });
  it('ignores caller-controlled array iteration methods when validating', () => {
    const value = evidence();
    const invalid = [{ private_provider_name: 'private-provider' }];
    Object.assign(value, {
      provider_calls: new Proxy(invalid, {
        get(target, name, receiver) {
          if (name === 'every') return () => true;
          return Reflect.get(target, name, receiver);
        },
      }),
    });
    expect(validateTraceAuctionEvidence(value)).toBe(false);
    expect(parseTraceAuctionTransport({ schema_version: 1, evidence: value })).toBeUndefined();
  });

  it('copies array elements without invoking caller-controlled map or length', () => {
    const value = evidence();
    const original = value.provider_calls;
    let customCalls = 0;
    value.provider_calls = new Proxy(original, {
      get(target, name, receiver) {
        if (name === 'map')
          return () => {
            customCalls += 1;
            return original;
          };
        if (name === 'length') {
          customCalls += 1;
          return 0;
        }
        return Reflect.get(target, name, receiver);
      },
    });
    const result = parseTraceAuctionTransport({ schema_version: 1, evidence: value });
    expect(result).toBeDefined();
    expect(customCalls).toBe(0);
    expect(Object.isFrozen(original)).toBe(false);
    expect(Object.isFrozen(original[0])).toBe(false);
    if (!result || result.evidence === undefined) throw new Error('should parse evidence');
    expect(result.evidence.provider_calls).not.toBe(original);
    expect(result.evidence.provider_calls[0]).not.toBe(original[0]);
    original[0].returned_bid_count = 99;
    expect(result.evidence.provider_calls[0].returned_bid_count).toBe(0);
  });
  it('keeps transport members exclusive at the public type boundary', () => {
    const record = evidence();
    if (!validateTraceAuctionEvidence(record)) throw new Error('should validate fixture');
    const both = {
      schema_version: 1,
      evidence: record,
      unavailable_reason: 'evidence_projection_failed',
    } as const;
    // @ts-expect-error Both transport alternatives cannot be present together.
    const invalid: TraceAuctionTransportV1 = both;
    expect(validateTraceAuctionTransport(invalid)).toBe(false);
  });
  it('copies and freezes every retained field without retaining source arrays', () => {
    const original = evidence();
    Object.assign(original, { total_time_ms: 0, terminal_reason: 'unknown' });
    Object.assign(original.provider_calls[0], { response_time_ms: 0 });
    Object.assign(original.slots[0], { selected_creative_size: [300, 250] });
    const envelope = { schema_version: 1, evidence: original };
    const result = parseTraceAuctionTransport(envelope);
    expect(result).toEqual(envelope);
    expect(result).not.toBe(envelope);
    if (!result || result.evidence === undefined) throw new Error('should parse evidence');
    expect(result.evidence).not.toBe(original);
    const assertFrozen = (value: unknown): void => {
      if (typeof value !== 'object' || value === null) return;
      expect(Object.isFrozen(value)).toBe(true);
      Object.values(value).forEach(assertFrozen);
    };
    assertFrozen(result);
    original.slots[0].requested_sizes[0][0] = 999;
    original.provider_calls[0].returned_bid_count = 99;
    expect(result.evidence.slots[0].requested_sizes[0]).toEqual([300, 250]);
    expect(result.evidence.provider_calls[0].returned_bid_count).toBe(0);
    expect(Object.isFrozen(original)).toBe(false);
  });

  it('copies an explicit unavailable transport and the exact correlation decision', () => {
    const unavailable = {
      schema_version: 1,
      unavailable_reason: 'evidence_projection_failed',
    };
    const result = parseTraceAuctionTransport(unavailable);
    expect(result).toEqual(unavailable);
    expect(Object.isFrozen(result)).toBe(true);
    const original = {
      schema_version: 1,
      diagnostic_auction_id: AUCTION,
      slot_ref: SLOT,
      runtime_slot_number: 1,
      request_number: 2,
    };
    const sidecar = parseTraceSlotCorrelation(original);
    expect(sidecar).toEqual(original);
    expect(Object.isFrozen(sidecar)).toBe(true);
    original.request_number = 3;
    expect(sidecar?.request_number).toBe(2);
  });

  it('rejects invalid inputs without serialization or getter invocation', () => {
    let calls = 0;
    const value = {
      schema_version: 1,
      evidence: evidence(),
      toJSON: () => {
        calls += 1;
        throw new Error('should not call');
      },
    };
    expect(parseTraceAuctionTransport(value)).toBeUndefined();
    expect(parseTraceSlotCorrelation(value)).toBeUndefined();
    expect(calls).toBe(0);
    expect(
      parseTraceAuctionTransport(
        new Proxy(
          {},
          {
            getPrototypeOf() {
              throw new Error('should reject');
            },
          }
        )
      )
    ).toBeUndefined();
  });

  it('copies stable own facts without invoking changing property reads', () => {
    let reads = 0;
    const source = new Proxy(evidence(), {
      get(target, name, receiver) {
        if (name === 'diagnostic_auction_id' && ++reads > 1) return 'private-id';
        return Reflect.get(target, name, receiver);
      },
    });
    expect(parseTraceAuctionTransport({ schema_version: 1, evidence: source })).toEqual({
      schema_version: 1,
      evidence: evidence(),
    });
    expect(reads).toBe(0);
  });
});

function evidence() {
  return {
    schema_version: 1,
    diagnostic_auction_id: AUCTION,
    source: 'initial_navigation_ssat',
    terminal_status: 'completed',
    provider_calls: [
      {
        provider_number: 1,
        role: 'bidder',
        status: 'success',
        returned_bid_count: 0,
      },
    ],
    slots: [
      {
        slot_number: 1,
        slot_ref: SLOT,
        requested_sizes: [[300, 250]],
        returned_bid_count: 0,
        candidate: 'no_candidate',
      },
    ],
    truncation: {
      omitted_provider_calls: 0,
      omitted_slots: 0,
      omitted_nested_values: 0,
    },
    coverage: { provider_to_slot_no_bid: 'unavailable' },
  };
}

describe('bounded server auction evidence contracts', () => {
  it('accepts zero-bid evidence, empty observations and numeric boundaries', () => {
    const value = evidence();
    expect(validateTraceAuctionEvidence(value)).toBe(true);
    expect(validateTraceAuctionEvidence({ ...value, provider_calls: [], slots: [] })).toBe(true);
    Object.assign(value, {
      total_time_ms: 4_294_967_295,
      terminal_reason: 'unknown',
    });
    Object.assign(value.provider_calls[0], {
      response_time_ms: 4_294_967_295,
      returned_bid_count: 65_535,
    });
    Object.assign(value.slots[0], {
      selected_creative_size: [100_000, 1],
      returned_bid_count: 65_535,
    });
    expect(validateTraceAuctionEvidence(value)).toBe(true);
  });

  it.each(['initial_navigation_ssat', 'spa_page_bids', 'auction_api'])(
    'accepts source %s',
    (source) => {
      expect(validateTraceAuctionEvidence({ ...evidence(), source })).toBe(true);
    }
  );
  it.each(['completed', 'execution_failed', 'dispatch_failed', 'abandoned', 'skipped'])(
    'accepts terminal status %s',
    (terminal_status) => {
      expect(validateTraceAuctionEvidence({ ...evidence(), terminal_status })).toBe(true);
    }
  );
  it.each([
    'policy_skipped',
    'no_eligible_slots',
    'no_provider_launched',
    'provider_execution_failed',
    'collection_failed',
    'unknown',
  ])('accepts terminal reason %s', (terminal_reason) => {
    expect(validateTraceAuctionEvidence({ ...evidence(), terminal_reason })).toBe(true);
  });
  it.each(['bidder', 'mediator', 'unknown'])('accepts provider role %s', (role) => {
    const value = evidence();
    Object.assign(value.provider_calls[0], { role });
    expect(validateTraceAuctionEvidence(value)).toBe(true);
  });
  it.each(['success', 'no_bid', 'error', 'pending', 'abandoned', 'unknown'])(
    'accepts provider status %s',
    (status) => {
      const value = evidence();
      Object.assign(value.provider_calls[0], { status });
      expect(validateTraceAuctionEvidence(value)).toBe(true);
    }
  );
  it.each(['selected', 'no_candidate', 'selected_unrenderable', 'unknown'])(
    'accepts candidate %s',
    (candidate) => {
      const value = evidence();
      Object.assign(value.slots[0], { candidate });
      expect(validateTraceAuctionEvidence(value)).toBe(true);
    }
  );

  it.each([
    { schema_version: 2 },
    { diagnostic_auction_id: 'internal-id-sentinel' },
    { source: 'initial_navigation' },
    { terminal_status: 'timeout' },
    { terminal_reason: 'raw-error-sentinel' },
    { total_time_ms: 4_294_967_296 },
    { total_time_ms: -1 },
    { total_time_ms: 0.5 },
    { total_time_ms: Infinity },
    { provider_calls: undefined },
    { coverage: { provider_to_slot_no_bid: 'known' } },
    { provider_name: 'fictional-private-provider' },
    { price: 1 },
    {
      truncation: {
        omitted_provider_calls: 65_536,
        omitted_slots: 0,
        omitted_nested_values: 0,
      },
    },
  ])('rejects extra fields, unsafe numbers and unknown enums %j', (fields) => {
    expect(validateTraceAuctionEvidence({ ...evidence(), ...fields })).toBe(false);
  });

  it('rejects unknown properties at every nested boundary', () => {
    for (const target of ['provider', 'slot', 'truncation', 'coverage'] as const) {
      const value = evidence();
      Object.assign(
        target === 'provider'
          ? value.provider_calls[0]
          : target === 'slot'
            ? value.slots[0]
            : value[target],
        { secret: 'private-sentinel' }
      );
      expect(validateTraceAuctionEvidence(value)).toBe(false);
    }
  });

  it.each([
    { provider_number: 0 },
    { provider_number: 65_536 },
    { provider_number: 1.5 },
    { role: 'named-provider' },
    { status: 'raw-error' },
    { returned_bid_count: -1 },
    { returned_bid_count: 65_536 },
    { response_time_ms: 4_294_967_296 },
  ])('rejects invalid provider fields %j', (fields) => {
    const value = evidence();
    Object.assign(value.provider_calls[0], fields);
    expect(validateTraceAuctionEvidence(value)).toBe(false);
  });

  it.each([
    { slot_number: 0 },
    { slot_number: 65_536 },
    { slot_ref: 'raw-slot-sentinel' },
    { candidate: 'filled' },
    { requested_sizes: [[0, 250]] },
    { requested_sizes: [[300, 0]] },
    { requested_sizes: [[100_001, 1]] },
    { requested_sizes: [[300.5, 250]] },
    { requested_sizes: [[300, 250, 1]] },
    { selected_creative_size: [0, 250] },
    { selected_creative_size: [300, 250, 1] },
    { returned_bid_count: 65_536 },
  ])('rejects invalid slot fields %j', (fields) => {
    const value = evidence();
    Object.assign(value.slots[0], fields);
    expect(validateTraceAuctionEvidence(value)).toBe(false);
  });

  it('rejects oversized inner models without truncating them', () => {
    const value = evidence();
    expect(
      validateTraceAuctionEvidence({
        ...value,
        provider_calls: Array.from({ length: 17 }, () => value.provider_calls[0]),
      })
    ).toBe(false);
    expect(
      validateTraceAuctionEvidence({
        ...value,
        slots: Array.from({ length: 65 }, () => value.slots[0]),
      })
    ).toBe(false);
    Object.assign(value.slots[0], {
      requested_sizes: Array.from({ length: 17 }, () => [300, 250]),
    });
    expect(validateTraceAuctionEvidence(value)).toBe(false);
  });

  it('does not invoke forbidden accessors or throw on proxy inspection', () => {
    const value = Object.defineProperty(evidence(), 'source', {
      enumerable: true,
      get: () => {
        throw new Error('private-sentinel');
      },
    });
    expect(validateTraceAuctionEvidence(value)).toBe(false);
    expect(
      validateTraceAuctionEvidence(
        new Proxy(
          {},
          {
            getPrototypeOf: () => {
              throw new Error('private-sentinel');
            },
          }
        )
      )
    ).toBe(false);
  });

  it('accepts exactly one evidence-or-unavailable transport member', () => {
    expect(
      validateTraceAuctionTransport({
        schema_version: 1,
        evidence: evidence(),
      })
    ).toBe(true);
    expect(
      validateTraceAuctionTransport({
        schema_version: 1,
        unavailable_reason: 'evidence_projection_failed',
      })
    ).toBe(true);
    for (const value of [
      {},
      { schema_version: 1 },
      { schema_version: 2, evidence: evidence() },
      {
        schema_version: 1,
        evidence: evidence(),
        unavailable_reason: 'evidence_projection_failed',
      },
      { schema_version: 1, unavailable_reason: 'private-parser-error' },
      { schema_version: 1, evidence: evidence(), secret: 'private-sentinel' },
    ])
      expect(validateTraceAuctionTransport(value)).toBe(false);
  });

  it('accepts an exact trace-only sidecar with safe positive browser sequence numbers', () => {
    const value = {
      schema_version: 1,
      diagnostic_auction_id: AUCTION,
      slot_ref: SLOT,
      runtime_slot_number: 1,
      request_number: Number.MAX_SAFE_INTEGER,
    };
    expect(validateTraceSlotCorrelation(value)).toBe(true);
    for (const fields of [
      { schema_version: 2 },
      { diagnostic_auction_id: 'private-auction-id' },
      { slot_ref: 'private-dom-id' },
      { runtime_slot_number: 0 },
      { request_number: 0 },
      { request_number: 0.5 },
      { request_number: Number.MAX_SAFE_INTEGER + 1 },
      { slotElementId: 'private-slot-sentinel' },
    ])
      expect(validateTraceSlotCorrelation({ ...value, ...fields })).toBe(false);
  });
});
