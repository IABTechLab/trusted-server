import { afterEach, describe, expect, it, vi } from 'vitest';

import type { AuctionSlot, TsjsApi } from '../../src/core/types';
import { installTraceRuntime } from '../../src/trace/runtime';

import { AUCTION_TOKEN, SLOT_TOKEN } from './fixtures';
import { gptTransport } from './gpt-fixtures';

function slot(ref: unknown = SLOT_TOKEN): AuctionSlot {
  return {
    id: 'private-id',
    gam_unit_path: '/private-path',
    div_id: 'private-div',
    formats: [[300, 250]],
    ext: { trusted_server: { trace_slot_ref: ref } },
  };
}
function setup(active: unknown = true) {
  const api = {} as TsjsApi;
  const scope = { tsjs: api, __tsjs_trace_active: active };
  const collector = installTraceRuntime(api, scope);
  return { api, collector };
}
afterEach(() => {
  vi.restoreAllMocks();
});

describe('shared SSAT/SPA slot identity bridge', () => {
  it('retains evidence but binds no prefix when delivered slots exceed the 64-slot cap', () => {
    const { api, collector } = setup();
    const first = slot();
    const slots = [first, ...Array.from({ length: 64 }, () => slot('private-invalid'))];
    // The tail may hide a duplicate, so a bounded inspection cannot bind even a valid prefix.
    api.traceGpt!.observeTransport(slots, gptTransport(), 'initial_navigation_ssat');
    expect(collector!.captureStatus()).toBe('complete');
    expect(collector!.snapshot().value?.serverAuctions).toHaveLength(1);
    expect(collector!.snapshot().value?.issues).toEqual(['correlation_unavailable']);
    expect(api.traceGpt!.identity(first)).toBeUndefined();
    expect(slots).toHaveLength(65);
    expect(slots[0]).toBe(first);
  });
  it('retains validated server capture when only delivered array descriptors are unreadable', () => {
    const { api, collector } = setup();
    const slots = new Proxy([slot()], {
      getOwnPropertyDescriptor: () => {
        throw new Error('private-descriptor');
      },
    });
    expect(() =>
      api.traceGpt!.observeTransport(slots, gptTransport(), 'initial_navigation_ssat')
    ).not.toThrow();
    expect(collector!.captureStatus()).toBe('complete');
    expect(collector!.snapshot().value?.issues).toEqual(['correlation_unavailable']);
  });
  it.each([false, undefined, 'true', 1])(
    'allocates no bridge or bindings for gate %s',
    (active) => {
      const api = {} as TsjsApi;
      const collector = installTraceRuntime(api, { tsjs: api, __tsjs_trace_active: active });
      expect(collector).toBeUndefined();
      expect(api.traceGpt).toBeUndefined();
    }
  );
  it('retains owned evidence and exact object bindings without changing slots or reading excluded fields', () => {
    const { api, collector } = setup();
    const original = slot();
    const unsafe = vi.fn(() => {
      throw new Error('private-field');
    });
    Object.defineProperty(original.ext!, 'private_extension', { get: unsafe });
    Object.defineProperty(original, 'private_field', { get: unsafe });
    const transport = gptTransport();
    const reads = vi.fn(() => {
      throw new Error('private-get');
    });
    const owned = new Proxy(transport, { get: reads });
    api.traceGpt!.observeTransport([original], owned, 'initial_navigation_ssat');
    expect(collector!.captureStatus()).toBe('complete');
    expect(api.traceGpt!.identity(original)).toEqual({
      diagnostic_auction_id: AUCTION_TOKEN,
      slot_ref: SLOT_TOKEN,
    });
    expect(api.traceGpt!.identity({ ...original })).toBeUndefined();
    expect(unsafe).not.toHaveBeenCalled();
    expect(reads).not.toHaveBeenCalled();
    expect(Object.isFrozen(original)).toBe(false);
    expect(Object.isFrozen(transport)).toBe(false);
    const identity = api.traceGpt!.identity(original)!;
    expect(Object.isFrozen(identity)).toBe(true);
    original.ext!.trusted_server!.trace_slot_ref = 'private-mutated';
    transport.evidence.slots[0]!.slot_ref = 'private-mutated';
    expect(identity.slot_ref).toBe(SLOT_TOKEN);
    expect(collector!.snapshot().value?.serverAuctions[0]!.slots[0]!.slot_ref).toBe(SLOT_TOKEN);
  });
  it.each([
    'missing',
    'malformed',
    'duplicate-delivered',
    'duplicate-evidence',
    'conflicting',
    'accessor',
    'throwing-descriptor',
  ] as const)('limits only correlation for a %s delivered reference', (kind) => {
    const { api, collector } = setup();
    const original = slot();
    const slots = [original];
    const transport = gptTransport();
    if (kind === 'missing') delete original.ext;
    if (kind === 'malformed') original.ext!.trusted_server!.trace_slot_ref = 'private-token';
    if (kind === 'duplicate-delivered') slots.push(slot());
    if (kind === 'duplicate-evidence')
      transport.evidence.slots.push({ ...transport.evidence.slots[0]!, slot_number: 2 });
    if (kind === 'conflicting')
      original.ext!.trusted_server!.trace_slot_ref = 'ts-slot-12345679-1234-4abc-8def-123456789abc';
    const getter = vi.fn(() => {
      throw new Error('private-reference');
    });
    if (kind === 'accessor')
      Object.defineProperty(original.ext!.trusted_server!, 'trace_slot_ref', { get: getter });
    if (kind === 'throwing-descriptor')
      original.ext = new Proxy(original.ext!, { getOwnPropertyDescriptor: getter });
    api.traceGpt!.observeTransport(slots, transport, 'initial_navigation_ssat');
    expect(collector!.captureStatus()).toBe('complete');
    expect(collector!.snapshot().value?.issues).toEqual(['correlation_unavailable']);
    expect(api.traceGpt!.identity(original)).toBeUndefined();
    expect(slots[0]).toBe(original);
    if (kind === 'accessor') expect(getter).not.toHaveBeenCalled();
  });
  it('preserves earlier collector capture but clears stale bindings when the next optional member is absent', () => {
    const { api, collector } = setup();
    const original = slot();
    api.traceGpt!.observeTransport([original], gptTransport(), 'initial_navigation_ssat');
    api.traceGpt!.observeTransport([original], undefined, 'initial_navigation_ssat');
    expect(api.traceGpt!.identity(original)).toBeUndefined();
    expect(collector!.snapshot().value?.serverAuctions).toHaveLength(1);
    expect(collector!.snapshot().value?.issues).toEqual([]);
  });
  it.each(['malformed', 'projection', 'api', 'wrong-source'] as const)(
    'keeps %s transport handling separate from correlation',
    (kind) => {
      const { api, collector } = setup();
      const original = slot();
      const value =
        kind === 'projection'
          ? { schema_version: 1, unavailable_reason: 'evidence_projection_failed' }
          : kind === 'malformed'
            ? { schema_version: 1 }
            : gptTransport(kind === 'api' ? 'auction_api' : 'spa_page_bids');
      api.traceGpt!.observeTransport([original], value, 'initial_navigation_ssat');
      expect(api.traceGpt!.identity(original)).toBeUndefined();
      expect(collector!.snapshot().value?.issues).toEqual([
        kind === 'projection' ? 'evidence_projection_failed' : 'evidence_validation_failed',
      ]);
    }
  );
  it('reads only an own data SPA transport member without caller getters or unrelated extensions', () => {
    const { api, collector } = setup();
    const original = slot();
    const getter = vi.fn(() => {
      throw new Error('private-member');
    });
    api.traceGpt!.observePageBids(Object.create({ trace_auction: gptTransport('spa_page_bids') }), [
      original,
    ]);
    expect(collector!.snapshot().value?.issues).toEqual([]);
    const response = { trace_auction: gptTransport('spa_page_bids') };
    api.traceGpt!.observePageBids(new Proxy(response, { get: getter }), [original]);
    expect(collector!.captureStatus()).toBe('complete');
    expect(getter).not.toHaveBeenCalled();
    api.traceGpt!.observePageBids(Object.defineProperty({}, 'trace_auction', { get: getter }), [
      original,
    ]);
    expect(collector!.snapshot().value?.issues).toEqual(['evidence_validation_failed']);
    expect(getter).not.toHaveBeenCalled();
  });
});
