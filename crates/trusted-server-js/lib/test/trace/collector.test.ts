import { describe, expect, it, vi } from 'vitest';

import { createTraceCollector } from '../../src/trace/collector';

import { SLOT_TOKEN } from './fixtures';

function transport(number = 1, source = 'initial_navigation_ssat') {
  return {
    schema_version: 1,
    evidence: {
      schema_version: 1,
      diagnostic_auction_id: token(number),
      source,
      terminal_status: 'completed',
      provider_calls: [],
      slots: [],
      truncation: { omitted_provider_calls: 0, omitted_slots: 0, omitted_nested_values: 0 },
      coverage: { provider_to_slot_no_bid: 'unavailable' },
    },
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
    request_number: number,
  };
}
function collector() {
  const result = createTraceCollector();
  if (typeof result !== 'object' || result === null) throw new Error('should create collector');
  return result;
}
describe('bounded memory-only trace collector', () => {
  it('cannot capture an invalid own enum disguised by a proxy getter', () => {
    const value = collector();
    const original = transport();
    original.evidence.terminal_status = 'private-invalid-enum';
    let reads = 0;
    original.evidence = new Proxy(original.evidence, {
      get(target, key, receiver) {
        reads += 1;
        if (key === 'terminal_status') return 'completed';
        return Reflect.get(target, key, receiver);
      },
    });
    value.recordTransport(original);
    expect(value.captureStatus()).toBe('unavailable');
    expect(value.snapshot().value?.issues).toEqual(['evidence_validation_failed']);
    expect(reads).toBe(0);
  });
  it('does not touch storage or start network requests during collection', () => {
    const get = vi.spyOn(Storage.prototype, 'getItem');
    const set = vi.spyOn(Storage.prototype, 'setItem');
    const remove = vi.spyOn(Storage.prototype, 'removeItem');
    const fetch = vi.fn();
    vi.stubGlobal('fetch', fetch);
    const value = collector();
    value.recordTransport(transport());
    value.recordCorrelation(sidecar());
    value.snapshot();
    expect(get).not.toHaveBeenCalled();
    expect(set).not.toHaveBeenCalled();
    expect(remove).not.toHaveBeenCalled();
    expect(fetch).not.toHaveBeenCalled();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });
  it('treats absent optional transport as no observation without resetting earlier evidence', () => {
    const value = collector();
    value.recordTransport(undefined);
    expect(value.captureStatus()).toBe('not_observed');
    value.recordTransport(transport());
    value.recordTransport(undefined);
    expect(value.captureStatus()).toBe('complete');
    expect(value.snapshot().value?.serverAuctions).toHaveLength(1);
  });
  it('records bounded capture failures in unique enum order without retaining input text', () => {
    const value = collector();
    value.recordTransportFailure();
    value.recordTransport(null);
    value.recordTransport({ schema_version: 1, unavailable_reason: 'evidence_projection_failed' });
    value.recordTransportFailure();
    expect(value.captureStatus()).toBe('unavailable');
    expect(value.snapshot().value?.issues).toEqual([
      'evidence_projection_failed',
      'evidence_transport_failed',
      'evidence_validation_failed',
    ]);
    value.recordTransport(transport());
    expect(value.captureStatus()).toBe('partial');
    expect(JSON.stringify(value.snapshot())).not.toContain('private');
  });
  it('retains newest sixteen records in observation order and exact eviction counts', () => {
    const value = collector();
    for (let number = 1; number <= 18; number += 1) value.recordTransport(transport(number));
    const snapshot = value.snapshot().value;
    expect(snapshot?.serverAuctions.map((record) => record.diagnostic_auction_id)).toEqual(
      Array.from({ length: 16 }, (_, i) => token(i + 3))
    );
    expect(snapshot?.omittedServerAuctions).toBe(2);
    expect(snapshot?.issues).toEqual(['record_evicted']);
    expect(value.captureStatus()).toBe('partial');
  });
  it('keeps sidecar-only loss and interpretation limits separate from server coverage', () => {
    const value = collector();
    value.recordTransport(transport());
    for (let number = 1; number <= 130; number += 1) value.recordCorrelation(sidecar(number));
    value.recordInterpretationIssue('external_client_side_unobservable');
    value.recordCorrelation({ private_value: 'private-sidecar' });
    expect(value.snapshot().value?.slotCorrelations).toHaveLength(128);
    expect(value.snapshot().value?.omittedSlotCorrelations).toBe(2);
    expect(value.snapshot().value?.issues).toEqual([
      'correlation_unavailable',
      'external_client_side_unobservable',
    ]);
    expect(value.captureStatus()).toBe('complete');
  });
  it('prunes and counts sidecars when their known unique auction is evicted', () => {
    const value = collector();
    value.recordTransport(transport(1));
    value.recordCorrelation(sidecar(1));
    for (let number = 2; number <= 17; number += 1) value.recordTransport(transport(number));
    expect(value.snapshot().value?.slotCorrelations).toEqual([]);
    expect(value.snapshot().value?.omittedSlotCorrelations).toBe(1);
    expect(value.snapshot().value?.issues).toEqual(['record_evicted', 'correlation_unavailable']);
  });
  it('keeps API records independent and declines any supplied API sidecar', () => {
    const value = collector();
    value.recordTransport(transport(1, 'auction_api'));
    value.recordCorrelation(sidecar(1));
    expect(value.snapshot().value?.serverAuctions).toHaveLength(1);
    expect(value.snapshot().value?.slotCorrelations).toEqual([]);
    expect(value.captureStatus()).toBe('complete');
    expect(value.snapshot().value?.issues).toEqual(['correlation_unavailable']);
  });
  it('returns fresh frozen arrays while caller objects remain mutable and isolated', () => {
    const value = collector();
    const original = transport();
    value.recordTransport(original);
    const first = value.snapshot();
    const second = value.snapshot();
    expect(first).toEqual(second);
    expect(first.value).not.toBe(second.value);
    expect(Object.isFrozen(first.value?.serverAuctions)).toBe(true);
    expect(Object.isFrozen(original.evidence)).toBe(false);
    original.evidence.diagnostic_auction_id = token(2);
    expect(first.value?.serverAuctions[0].diagnostic_auction_id).toBe(token(1));
  });
  it('invalidates capture on checked eviction overflow without wrapping or throwing into ads', () => {
    const value = collector();
    const record = transport();
    for (let count = 0; count < 65535 + 17; count += 1) value.recordTransport(record);
    expect(value.snapshot()).toEqual({ ok: false, reason: 'omission_counter_overflow' });
  }, 15000);
  it('ignores late callbacks after destruction and removes retained local evidence', () => {
    const value = collector();
    value.recordTransport(transport());
    value.destroy();
    value.recordTransport(transport());
    value.recordCorrelation(sidecar());
    value.recordTransportFailure();
    expect(value.snapshot().value?.serverAuctions).toEqual([]);
    expect(value.snapshot().value?.slotCorrelations).toEqual([]);
    expect(value.captureStatus()).toBe('not_observed');
  });
});
