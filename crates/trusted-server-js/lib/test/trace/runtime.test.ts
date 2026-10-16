import { describe, expect, it, vi } from 'vitest';

import type { TsjsApi } from '../../src/core/types';
import { buildAdRequest } from '../../src/core/auction';
import {
  getActiveTraceCollector,
  installTraceRuntime,
  prepareTraceAuctionRequest,
} from '../../src/trace/runtime';

function setup(active: unknown = true) {
  const api = {} as TsjsApi;
  const randomUUID = vi.fn(() => '12345678-1234-4abc-8def-123456789abc');
  const scope = { tsjs: api, __tsjs_trace_active: active, crypto: { randomUUID } };
  return { api, scope, randomUUID };
}
function request() {
  return buildAdRequest([
    {
      code: 'example-slot',
      mediaTypes: { banner: { sizes: [[300, 250]] } },
      bidder: 'example-bidder',
    },
    {
      adUnitCode: 'example-slot',
      mediaTypes: { banner: { sizes: [[320, 50]] } },
      bidder: 'example-other',
    },
  ]);
}
describe('one strictly gated page trace facade', () => {
  it.each([undefined, false, 'true', 1, null])(
    'does not activate or mint tokens for nonliteral gate %s',
    (active) => {
      const fixture = setup();
      fixture.scope.__tsjs_trace_active = active;
      expect(installTraceRuntime(fixture.api, fixture.scope)).toBeUndefined();
      expect(getActiveTraceCollector(fixture.scope)).toBeUndefined();
      expect(prepareTraceAuctionRequest(request(), fixture.scope)).toBeUndefined();
      expect(fixture.randomUUID).not.toHaveBeenCalled();
      expect(Object.keys(fixture.api)).toEqual([]);
    }
  );
  it('reuses the page facade across independent module instances without storage access', async () => {
    const fixture = setup();
    const storage = vi.spyOn(Storage.prototype, 'getItem');
    const first = installTraceRuntime(fixture.api, fixture.scope);
    expect(first).toBeDefined();
    expect(getActiveTraceCollector(fixture.scope)).toBe(first);
    vi.resetModules();
    const independent = await import('../../src/trace/runtime');
    expect(independent.installTraceRuntime(fixture.api, fixture.scope)).toBe(first);
    expect(storage).not.toHaveBeenCalled();
    storage.mockRestore();
  });
  it('mints one opaque ref after final grouping and preserves original input payload', () => {
    const fixture = setup();
    installTraceRuntime(fixture.api, fixture.scope);
    const original = request();
    const before = structuredClone(original);
    const result = prepareTraceAuctionRequest(original, fixture.scope);
    expect(result).toMatchObject({
      request: {
        adUnits: [
          {
            code: 'example-slot',
            ext: {
              trusted_server: { trace_slot_ref: 'ts-slot-12345678-1234-4abc-8def-123456789abc' },
            },
          },
        ],
      },
      slotRefs: ['ts-slot-12345678-1234-4abc-8def-123456789abc'],
    });
    expect(fixture.randomUUID).toHaveBeenCalledTimes(1);
    expect(original).toEqual(before);
  });
  it('returns an observer without client refs if Web Crypto is unavailable', () => {
    const fixture = setup();
    installTraceRuntime(fixture.api, fixture.scope);
    const scope = { ...fixture.scope, crypto: undefined };
    const original = request();
    expect(prepareTraceAuctionRequest(original, scope)).toMatchObject({
      request: original,
      slotRefs: [],
    });
  });
  it('fails open when token generation throws or returns malformed or duplicate tokens', () => {
    const fixture = setup();
    installTraceRuntime(fixture.api, fixture.scope);
    const original = request();
    fixture.randomUUID.mockImplementation(() => {
      throw new Error('private-random-error');
    });
    expect(prepareTraceAuctionRequest(original, fixture.scope)).toMatchObject({
      request: original,
      slotRefs: [],
    });
    fixture.randomUUID.mockReturnValue('private-invalid-token');
    expect(prepareTraceAuctionRequest(original, fixture.scope)).toMatchObject({
      request: original,
      slotRefs: [],
    });
    fixture.randomUUID.mockReturnValue('12345678-1234-4abc-8def-123456789abc');
    const two = buildAdRequest([{ code: 'example-one' }, { code: 'example-two' }]);
    expect(prepareTraceAuctionRequest(two, fixture.scope)).toMatchObject({
      request: two,
      slotRefs: [],
    });
    expect(original.adUnits[0]).not.toHaveProperty('ext');
  });
  it('does not create a new collector from an auction caller before core initialization', () => {
    const fixture = setup();
    expect(prepareTraceAuctionRequest(request(), fixture.scope)).toBeUndefined();
    expect(fixture.randomUUID).not.toHaveBeenCalled();
    expect(Object.keys(fixture.api)).toEqual([]);
  });
});
