import { afterEach, describe, expect, it, vi } from 'vitest';

import { createTraceCollector } from '../../src/trace/collector';
import { createTracePending, pendingExpiry } from '../../src/trace/pending';

import { gptTransport } from './gpt-fixtures';
import { SLOT_TOKEN } from './fixtures';

function carry() {
  return { collector: createTraceCollector(), slotRefs: [SLOT_TOKEN] };
}
function response() {
  return {
    ext: {
      trusted_server: {
        trace_auction: {
          ...gptTransport(),
          evidence: { ...gptTransport().evidence!, source: 'auction_api' },
        },
      },
    },
  };
}
function bindings(bidId = 'example-bid', bidderRequestId = 'example-request') {
  return [{ bidId, bidderRequestId, slotRef: SLOT_TOKEN }];
}
function identities(bidIds: readonly string[], bidderRequestId = 'example-request') {
  return bidIds.map((bidId) => ({ bidId, bidderRequestId }));
}
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});
describe('bounded Prebid trace pending records', () => {
  it.each([0, 2000, 2 ** 31 - 1 - 5000])('captures timeout %s once', (timeout) => {
    expect(pendingExpiry(100, timeout)).toBe(100 + timeout + 5000);
  });
  it.each([undefined, -1, 1.5, NaN, Infinity, '2000', 2 ** 31 - 5000])(
    'defaults invalid timeout %s to 3000',
    (timeout) => {
      expect(pendingExpiry(100, timeout)).toBe(8100);
    }
  );
  it.each([-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER])('declines unsafe clock %s', (now) => {
    expect(pendingExpiry(now, 3000)).toBeUndefined();
  });
  it.each([0, 2000, 2 ** 31 - 1 - 5000])(
    'arms one bounded browser timer for configured timeout %s',
    (timeout) => {
      vi.useFakeTimers();
      vi.setSystemTime(0);
      const schedule = vi.fn((callback: () => void, delay: number) => setTimeout(callback, delay));
      const pending = createTracePending({ timeout: () => timeout, schedule });
      const value = carry();
      const handle = pending.add(bindings(), value);
      expect(schedule.mock.calls[0]![1]).toBe(timeout + 5000);
      vi.advanceTimersByTime(timeout + 4999);
      pending.response(handle, response());
      expect(value.collector.captureStatus()).toBe('complete');
      expect(vi.getTimerCount()).toBe(0);
    }
  );
  it('records readable API evidence only once and never emits GPT sidecars', () => {
    const pending = createTracePending({ now: () => 0 });
    const value = carry();
    const handle = pending.add(bindings(), value);
    pending.response(handle, response());
    pending.response(handle, response());
    pending.failure(identities(['example-bid']));
    expect(value.collector.snapshot().value?.serverAuctions).toHaveLength(1);
    expect(value.collector.snapshot().value?.slotCorrelations).toEqual([]);
    expect(value.collector.captureStatus()).toBe('complete');
    pending.destroy();
  });
  it('consumes matching original IDs before callback and isolates concurrent requests', () => {
    const pending = createTracePending({ now: () => 0 });
    const a = carry();
    const b = carry();
    pending.add(bindings('first'), a);
    const second = pending.add(bindings('second'), b);
    pending.failure(identities(['first']));
    pending.failure(identities(['first']));
    pending.response(second, response());
    expect(a.collector.captureStatus()).toBe('unavailable');
    expect(b.collector.captureStatus()).toBe('complete');
    pending.destroy();
  });
  it('retires expiry without inventing a transport outcome and captures timeout only at creation', () => {
    vi.useFakeTimers();
    let timeout = 0;
    const readTimeout = vi.fn(() => timeout);
    const pending = createTracePending({ now: () => Date.now(), timeout: readTimeout });
    const value = carry();
    const handle = pending.add(bindings(), value);
    timeout = 100000;
    vi.advanceTimersByTime(5000);
    pending.failure(identities(['example-bid']));
    pending.response(handle, response());
    expect(readTimeout).toHaveBeenCalledTimes(1);
    expect(value.collector.captureStatus()).toBe('not_observed');
    expect(vi.getTimerCount()).toBe(0);
  });
  it('checks expiry synchronously when a delayed callback beats its timer', () => {
    let now = 0;
    const value = carry();
    const pending = createTracePending({ now: () => now, timeout: () => 0 });
    const handle = pending.add(bindings(), value);
    now = 5000;
    pending.response(handle, response());
    expect(value.collector.captureStatus()).toBe('not_observed');
    pending.destroy();
  });
  it('evicts the oldest of 128 records without failures and destroys all timers', () => {
    vi.useFakeTimers();
    const pending = createTracePending({ now: () => Date.now() });
    const values = Array.from({ length: 129 }, carry);
    const handles = values.map((value, index) => pending.add(bindings(`bid-${index}`), value));
    expect(vi.getTimerCount()).toBe(128);
    pending.response(handles[0], response());
    expect(values[0].collector.captureStatus()).toBe('not_observed');
    pending.destroy();
    expect(vi.getTimerCount()).toBe(0);
    pending.failure(identities(['bid-1']));
    expect(values[1].collector.captureStatus()).toBe('not_observed');
    expect(pending.add(bindings(), carry())).toBeUndefined();
  });
  it('owns bindings/refs and removes state before a throwing/reentrant diagnostic callback', () => {
    const pending = createTracePending({ now: () => 0 });
    const collector = createTraceCollector();
    const failure = vi.fn(() => {
      pending.failure(identities(['original']));
      throw new Error('private-error');
    });
    const value = {
      collector: { ...collector, recordTransportFailure: failure },
      slotRefs: [SLOT_TOKEN],
    };
    const list = bindings('original');
    pending.add(list, value);
    list[0].bidId = 'mutated';
    value.slotRefs[0] = 'mutated';
    expect(() => pending.failure(identities(['original']))).not.toThrow();
    expect(failure).toHaveBeenCalledTimes(1);
    pending.destroy();
  });
  it('declines clock overflow and ambiguous IDs without disturbing previous capture', () => {
    let now = 0;
    const pending = createTracePending({ now: () => now });
    const value = carry();
    value.collector.recordTransport(response().ext.trusted_server.trace_auction);
    pending.add(bindings(), value);
    expect(pending.add(bindings(), carry())).toBeDefined();
    now = Number.MAX_SAFE_INTEGER;
    expect(pending.add(bindings('new'), value)).toBeUndefined();
    now = 0;
    pending.failure(identities(['example-bid']));
    expect(value.collector.captureStatus()).toBe('complete');
    pending.destroy();
  });
  it('keeps exact response handles for every collided request while declining ambiguous hooks', () => {
    vi.useFakeTimers();
    const pending = createTracePending({ now: () => Date.now() });
    const a = carry();
    const b = carry();
    const c = carry();
    const first = pending.add(bindings('first'), a);
    const second = pending.add(bindings('second'), b);
    const collision = pending.add([...bindings('first'), ...bindings('second')], c);
    expect(collision).toBeDefined();
    expect(vi.getTimerCount()).toBe(3);
    pending.failure(identities(['first', 'second']));
    expect([a, b, c].map((value) => value.collector.captureStatus())).toEqual([
      'not_observed',
      'not_observed',
      'not_observed',
    ]);
    pending.response(first, response());
    pending.response(second, response());
    pending.response(collision, response());
    expect([a, b, c].map((value) => value.collector.captureStatus())).toEqual([
      'complete',
      'complete',
      'complete',
    ]);
    expect(vi.getTimerCount()).toBe(0);
    pending.destroy();
  });
  it('keeps over-cap responses and disables current and later hooks that could collide with unseen IDs', () => {
    vi.useFakeTimers();
    const pending = createTracePending({ now: () => Date.now() });
    const a = carry();
    const b = carry();
    const c = carry();
    const first = pending.add(bindings('bid-2048'), a);
    const overflow = pending.add(
      Array.from({ length: 2049 }, (_, index) => bindings(`bid-${index}`)[0]!),
      b
    );
    const later = pending.add(bindings('bid-2047'), c);
    expect(overflow).toBeDefined();
    expect(later).toBeDefined();
    pending.failure(identities(['bid-2048', 'bid-2047']));
    expect([a, b, c].map((value) => value.collector.captureStatus())).toEqual([
      'not_observed',
      'not_observed',
      'not_observed',
    ]);
    pending.response(first, response());
    pending.response(overflow, response());
    pending.response(later, response());
    expect([a, b, c].map((value) => value.collector.captureStatus())).toEqual([
      'complete',
      'complete',
      'complete',
    ]);
    expect(vi.getTimerCount()).toBe(0);
  });
  it.each(['response', 'failure'])(
    'isolates a reused original ID after old %s retirement',
    (retire) => {
      vi.useFakeTimers();
      const pending = createTracePending({ now: () => Date.now() });
      const old = carry();
      const next = carry();
      const first = pending.add(bindings('reused', 'request-old'), old);
      if (retire === 'response') pending.response(first, response());
      else pending.failure(identities(['reused'], 'request-old'));
      const second = pending.add(bindings('reused', 'request-new'), next);
      pending.failure(identities(['reused'], 'request-old'));
      expect(next.collector.captureStatus()).toBe('not_observed');
      expect(vi.getTimerCount()).toBe(1);
      pending.response(second, response());
      expect(next.collector.captureStatus()).toBe('complete');
      expect(vi.getTimerCount()).toBe(0);
    }
  );
  it.each([undefined, '', 1, 'x'.repeat(129)])(
    'keeps malformed request ID %s response-only',
    (bidderRequestId) => {
      vi.useFakeTimers();
      const pending = createTracePending({ now: () => Date.now() });
      const value = carry();
      const handle = pending.add(
        [{ bidId: 'reused', bidderRequestId } as ReturnType<typeof bindings>[number]],
        value
      );
      pending.failure([
        { bidId: 'reused', bidderRequestId } as ReturnType<typeof bindings>[number],
      ]);
      expect(value.collector.captureStatus()).toBe('not_observed');
      expect(value.collector.snapshot().value?.issues).toEqual(['correlation_unavailable']);
      pending.response(handle, response());
      expect(value.collector.captureStatus()).toBe('complete');
      expect(vi.getTimerCount()).toBe(0);
    }
  );
});
